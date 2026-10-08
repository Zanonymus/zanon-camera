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
    let img = RgbImage::from_raw(frame.width, frame.height, frame.rgb).ok_or(Error::BadFrame)?;
    let img = match rotation {
        Rotation::None => img,
        Rotation::Cw90 => imageops::rotate90(&img),
        Rotation::Cw180 => imageops::rotate180(&img),
        Rotation::Cw270 => imageops::rotate270(&img),
    };
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
    fn rejects_mismatched_frame() {
        let bad = Frame { width: 4, height: 4, rgb: vec![0; 5] };
        assert!(encode_jpeg(bad, Rotation::None, 90).is_err());
    }
}
