use std::path::{Path, PathBuf};

use v4l::buffer::Type;
use v4l::capability::Flags;
use v4l::io::mmap::Stream;
use v4l::video::Capture;
use v4l::{Device, FourCC};
use zanon_camera_core::capture::Frame;

const MJPG: [u8; 4] = *b"MJPG";
const YUYV: [u8; 4] = *b"YUYV";
const NV12: [u8; 4] = *b"NV12";

pub struct Opened {
    pub dev: Device,
    pub fourcc: [u8; 4],
    pub width: u32,
    pub height: u32,
    pub stride: usize,
}

pub fn candidates() -> Vec<PathBuf> {
    let mut nodes: Vec<(u32, PathBuf)> = std::fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let n = name.strip_prefix("video")?.parse().ok()?;
            Some((n, e.path()))
        })
        .collect();
    nodes.sort();
    nodes.into_iter().map(|(_, p)| p).collect()
}

fn cc_name(cc: [u8; 4]) -> String {
    String::from_utf8_lossy(&cc).into_owned()
}

pub fn try_open(path: &Path) -> Result<Opened, String> {
    let p = path.display();
    let dev = Device::with_path(path).map_err(|e| format!("{p}: cannot open ({e})"))?;
    let caps = dev.query_caps().map_err(|e| format!("{p}: cannot query ({e})"))?;
    if !caps.capabilities.contains(Flags::VIDEO_CAPTURE) {
        let hint = if caps.capabilities.contains(Flags::VIDEO_CAPTURE_MPLANE) {
            "multi-planar camera, needs libcamera (not supported yet)"
        } else {
            "not a video capture node"
        };
        return Err(format!("{p} [{} / {}]: {hint}", caps.driver, caps.card));
    }
    let offered: Vec<[u8; 4]> = dev
        .enum_formats()
        .map_err(|e| format!("{p} [{}]: cannot list formats ({e})", caps.card))?
        .into_iter()
        .map(|f| f.fourcc.repr)
        .collect();
    for want in [MJPG, YUYV, NV12] {
        if !offered.contains(&want) {
            continue;
        }
        let Ok(mut fmt) = dev.format() else { continue };
        fmt.fourcc = FourCC::new(&want);
        fmt.width = 1280;
        fmt.height = 720;
        match dev.set_format(&fmt) {
            Ok(got) if got.fourcc.repr == want => {
                return Ok(Opened {
                    dev,
                    fourcc: want,
                    width: got.width,
                    height: got.height,
                    stride: got.stride as usize,
                })
            }
            _ => {}
        }
    }
    let list: Vec<String> = offered.into_iter().map(cc_name).collect();
    Err(format!("{p} [{}]: no supported pixel format (offers {})", caps.card, list.join(", ")))
}

pub fn open_stream(o: &Opened) -> Result<Stream<'_>, String> {
    Stream::with_buffers(&o.dev, Type::VideoCapture, 4).map_err(|e| format!("cannot start streaming ({e})"))
}

pub fn decode(o: &Opened, data: &[u8]) -> Option<Frame> {
    let (w, h) = (o.width as usize, o.height as usize);
    match o.fourcc {
        MJPG => Frame::from_mjpeg(data),
        YUYV => {
            let row = w * 2;
            if o.stride <= row {
                Frame::from_yuyv(o.width, o.height, data)
            } else {
                let packed: Vec<u8> = (0..h).flat_map(|y| data.get(y * o.stride..y * o.stride + row)).flatten().copied().collect();
                Frame::from_yuyv(o.width, o.height, &packed)
            }
        }
        _ => {
            let stride = o.stride.max(w);
            let uv = stride * h;
            if data.len() < uv + stride * h / 2 {
                return None;
            }
            Some(Frame::from_yuv420(o.width, o.height, 1, &data[..uv], stride, &data[uv..], &data[uv + 1..], stride, 2))
        }
    }
}
