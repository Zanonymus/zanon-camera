slint::include_modules!();

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use slint::{Rgb8Pixel, SharedPixelBuffer};
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};
use zanon_camera_core::capture::{encode_jpeg, Frame, Rotation};
use zanon_camera_core::qr;

type Latest = Arc<Mutex<Option<Frame>>>;

fn open_camera() -> Result<(Device, FourCC, u32, u32), String> {
    let dev = Device::new(0).map_err(|e| format!("no camera at /dev/video0: {e}"))?;
    let mut fmt = dev.format().map_err(|e| e.to_string())?;
    fmt.width = 1280;
    fmt.height = 720;
    for cc in [b"MJPG", b"YUYV"] {
        fmt.fourcc = FourCC::new(cc);
        if let Ok(got) = dev.set_format(&fmt) {
            if got.fourcc == FourCC::new(cc) {
                return Ok((dev, got.fourcc, got.width, got.height));
            }
        }
    }
    Err("camera offers neither MJPG nor YUYV".into())
}

fn next_path() -> PathBuf {
    let dir = std::env::var_os("XDG_PICTURES_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Pictures")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Camera");
    let _ = std::fs::create_dir_all(&dir);
    (1..)
        .map(|n| dir.join(format!("IMG_{n:05}.jpg")))
        .find(|p| !p.exists())
        .unwrap()
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let latest: Latest = Arc::default();

    {
        let latest = latest.clone();
        ui.on_shutter(move || {
            let Some(frame) = latest.lock().unwrap().take() else { return };
            match encode_jpeg(frame, Rotation::None, 92) {
                Ok(jpeg) => {
                    let path = next_path();
                    let _ = std::fs::write(&path, jpeg);
                    eprintln!("saved {}", path.display());
                }
                Err(e) => eprintln!("encode failed: {e}"),
            }
        });
    }

    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let (dev, fourcc, w, h) = match open_camera() {
            Ok(c) => c,
            Err(msg) => {
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_status(msg.into()));
                return;
            }
        };
        let mut stream = match Stream::with_buffers(&dev, Type::VideoCapture, 4) {
            Ok(s) => s,
            Err(e) => {
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_status(e.to_string().into()));
                return;
            }
        };
        let mut n = 0u32;
        while let Ok((buf, meta)) = stream.next() {
            let data = &buf[..meta.bytesused as usize];
            let frame = if fourcc == FourCC::new(b"MJPG") {
                Frame::from_mjpeg(data)
            } else {
                Frame::from_yuyv(w, h, data)
            };
            let Some(frame) = frame else { continue };
            n += 1;
            let code = if n % 5 == 0 {
                qr::scan_gray(frame.width as usize, frame.height as usize, &frame.gray())
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let pixels = SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
                &frame.rgb, frame.width, frame.height,
            );
            *latest.lock().unwrap() = Some(frame);
            let scanned = n % 5 == 0;
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_preview(slint::Image::from_rgb8(pixels));
                if scanned {
                    ui.set_qr_text(code.into());
                }
            });
        }
    });

    ui.run()
}
