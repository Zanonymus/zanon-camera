use image::codecs::jpeg::JpegEncoder;
use image::imageops;
use image::RgbImage;

/// Clockwise rotation needed to make a sensor frame upright, from the device orientation at capture time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    None,
    Cw90,
    Cw180,
    Cw270,
}

/// A raw RGB8 frame straight from the camera, row-major, no container and no metadata.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

#[derive(Debug)]
pub enum Error {
    BadFrame,
    Encode(image::ImageError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadFrame => write!(f, "frame size does not match its pixel data"),
            Error::Encode(e) => write!(f, "encode: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Turns a raw frame into a JPEG with the rotation baked into the pixels. The file is built only
/// from pixel data, so it never contains EXIF, GPS, XMP, ICC, thumbnails, comments or timestamps.
pub fn encode_jpeg(frame: Frame, rotation: Rotation, quality: u8) -> Result<Vec<u8>, Error> {
    let img = frame.rotated(rotation)?;
    let img = RgbImage::from_raw(img.width, img.height, img.rgb).ok_or(Error::BadFrame)?;
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&img)
        .map_err(Error::Encode)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32) -> Frame {
        Frame { width: w, height: h, rgb: vec![120; (w * h * 3) as usize] }
    }

    /// Marker bytes of every segment before the scan data.
    fn markers(jpeg: &[u8]) -> Vec<u8> {
        let mut m = vec![];
        let mut i = 2;
        while i + 4 <= jpeg.len() {
            assert_eq!(jpeg[i], 0xFF);
            let marker = jpeg[i + 1];
            m.push(marker);
            if marker == 0xDA {
                break;
            }
            i += 2 + u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        }
        m
    }

    #[test]
    fn rotation_is_baked_into_pixels() {
        let out = encode_jpeg(frame(40, 20), Rotation::Cw90, 90).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (20, 40));
        let out = encode_jpeg(frame(40, 20), Rotation::Cw180, 90).unwrap();
        assert_eq!(image::load_from_memory(&out).unwrap().width(), 40);
    }

    #[test]
    fn output_has_no_metadata_segments() {
        let out = encode_jpeg(frame(32, 32), Rotation::Cw270, 90).unwrap();
        for m in markers(&out) {
            assert!(!(0xE1..=0xEF).contains(&m) && m != 0xFE, "metadata segment {m:#x}");
        }
        assert!(exif::Reader::new()
            .read_from_container(&mut std::io::Cursor::new(&out))
            .is_err());
    }

    #[test]
    fn yuyv_gray_and_mjpeg() {
        let f = Frame::from_yuyv(4, 2, &[128; 16]).unwrap();
        assert_eq!(f.rgb.len(), 24);
        assert!(f.rgb.iter().all(|&v| (126..=130).contains(&v)));
        assert_eq!(f.gray().len(), 8);
        assert!(Frame::from_yuyv(4, 2, &[0; 3]).is_none());
        let jpg = encode_jpeg(frame(16, 16), Rotation::None, 80).unwrap();
        assert_eq!(Frame::from_mjpeg(&jpg).unwrap().width, 16);
    }

    #[test]
    fn yuv420_with_strides() {
        // 4x2 image, row stride 6 (padding), interleaved chroma with pixel stride 2
        let y = [100u8; 12];
        let uv = [128u8; 8];
        let f = Frame::from_yuv420(4, 2, 1, &y, 6, &uv, &uv, 4, 2);
        assert_eq!((f.width, f.height, f.rgb.len()), (4, 2, 24));
        let small = Frame::from_yuv420(4, 2, 2, &y, 6, &uv, &uv, 4, 2);
        assert_eq!((small.width, small.height), (2, 1));
        assert!(f.rgb.iter().all(|&v| v == 100));
    }

    #[test]
    fn rejects_mismatched_frame() {
        let bad = Frame { width: 4, height: 4, rgb: vec![0; 5] };
        assert!(encode_jpeg(bad, Rotation::None, 90).is_err());
    }
}

impl Frame {
    /// Returns the frame with `rotation` applied to the pixels.
    pub fn rotated(self, rotation: Rotation) -> Result<Frame, Error> {
        if rotation == Rotation::None {
            return Ok(self);
        }
        let img = RgbImage::from_raw(self.width, self.height, self.rgb).ok_or(Error::BadFrame)?;
        let img = match rotation {
            Rotation::None => img,
            Rotation::Cw90 => imageops::rotate90(&img),
            Rotation::Cw180 => imageops::rotate180(&img),
            Rotation::Cw270 => imageops::rotate270(&img),
        };
        Ok(Frame { width: img.width(), height: img.height(), rgb: img.into_raw() })
    }

    /// Android YUV_420_888 planes (arbitrary strides) to RGB8, sampling every `step`-th pixel
    /// so previews can be made cheaply.
    #[allow(clippy::too_many_arguments)]
    pub fn from_yuv420(
        width: u32,
        height: u32,
        step: u32,
        y: &[u8],
        y_row_stride: usize,
        u: &[u8],
        v: &[u8],
        uv_row_stride: usize,
        uv_pixel_stride: usize,
    ) -> Frame {
        let (ow, oh) = (width / step, height / step);
        let mut rgb = Vec::with_capacity((ow * oh * 3) as usize);
        for oy in 0..oh {
            let py = (oy * step) as usize;
            for ox in 0..ow {
                let px = (ox * step) as usize;
                let yy = y[py * y_row_stride + px] as i32;
                let ci = (py / 2) * uv_row_stride + (px / 2) * uv_pixel_stride;
                let (cu, cv) = (u[ci] as i32 - 128, v[ci] as i32 - 128);
                let c = |v: i32| v.clamp(0, 255) as u8;
                rgb.push(c(yy + ((359 * cv) >> 8)));
                rgb.push(c(yy - ((88 * cu + 183 * cv) >> 8)));
                rgb.push(c(yy + ((454 * cu) >> 8)));
            }
        }
        Frame { width: ow, height: oh, rgb }
    }

    /// Packed YUYV (YUY2) 4:2:2 from a V4L2 webcam to RGB8.
    pub fn from_yuyv(width: u32, height: u32, yuyv: &[u8]) -> Option<Frame> {
        if yuyv.len() < (width * height * 2) as usize || width % 2 != 0 {
            return None;
        }
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        let clamp = |v: i32| v.clamp(0, 255) as u8;
        for px in yuyv[..(width * height * 2) as usize].chunks_exact(4) {
            let (y0, u, y1, v) = (px[0] as i32, px[1] as i32 - 128, px[2] as i32, px[3] as i32 - 128);
            for y in [y0, y1] {
                rgb.push(clamp(y + ((359 * v) >> 8)));
                rgb.push(clamp(y - ((88 * u + 183 * v) >> 8)));
                rgb.push(clamp(y + ((454 * u) >> 8)));
            }
        }
        Some(Frame { width, height, rgb })
    }

    /// One MJPEG frame from a camera, decoded to RGB8 (any metadata in the source is dropped here).
    pub fn from_mjpeg(data: &[u8]) -> Option<Frame> {
        let img = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).ok()?;
        let rgb = img.to_rgb8();
        Some(Frame { width: rgb.width(), height: rgb.height(), rgb: rgb.into_raw() })
    }

    /// Cheap nearest-neighbour thumbnail whose longer side is at most `max` pixels.
    pub fn thumbnail(&self, max: u32) -> Frame {
        let step = (self.width.max(self.height) / max).max(1);
        let (ow, oh) = (self.width / step, self.height / step);
        let mut rgb = Vec::with_capacity((ow * oh * 3) as usize);
        for y in 0..oh {
            for x in 0..ow {
                let i = (((y * step) * self.width + x * step) * 3) as usize;
                rgb.extend_from_slice(&self.rgb[i..i + 3]);
            }
        }
        Frame { width: ow, height: oh, rgb }
    }

    /// 8-bit luma for QR scanning.
    pub fn gray(&self) -> Vec<u8> {
        self.rgb
            .chunks_exact(3)
            .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
            .collect()
    }
}
