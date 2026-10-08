use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

#[derive(Debug)]
pub enum Error {
    Decode(image::ImageError),
    Encode(image::ImageError),
    UnsupportedFormat,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Decode(e) => write!(f, "decode: {e}"),
            Error::Encode(e) => write!(f, "encode: {e}"),
            Error::UnsupportedFormat => write!(f, "unsupported image format"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub jpeg_quality: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self { jpeg_quality: 92 }
    }
}

/// Decodes an image, bakes its EXIF orientation into the pixels and re-encodes it
/// from raw pixels only, so no metadata (EXIF, GPS, XMP, ICC, thumbnails, comments)
/// can survive. The output format matches the input (JPEG, PNG or WebP).
pub fn sanitize_image(input: &[u8], opts: Options) -> Result<Vec<u8>, Error> {
    let reader = ImageReader::new(Cursor::new(input))
        .with_guessed_format()
        .map_err(|e| Error::Decode(e.into()))?;
    let format = reader.format().ok_or(Error::UnsupportedFormat)?;
    let mut decoder = reader.into_decoder().map_err(Error::Decode)?;
    let orientation = decoder.orientation().map_err(Error::Decode)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(Error::Decode)?;
    img.apply_orientation(orientation);

    let mut out = Vec::with_capacity(input.len());
    match format {
        ImageFormat::Jpeg => {
            let rgb = img.to_rgb8();
            JpegEncoder::new_with_quality(&mut out, opts.jpeg_quality)
                .encode_image(&rgb)
                .map_err(Error::Encode)?;
        }
        ImageFormat::Png | ImageFormat::WebP => {
            img.write_to(&mut Cursor::new(&mut out), format)
                .map_err(Error::Encode)?;
        }
        _ => return Err(Error::UnsupportedFormat),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbImage;

    /// JPEG with an APP1 EXIF segment holding the given orientation and a GPS-like marker string.
    fn jpeg_with_exif(w: u32, h: u32, orientation: u16) -> Vec<u8> {
        let mut plain = Vec::new();
        JpegEncoder::new_with_quality(&mut plain, 90)
            .encode_image(&RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30])))
            .unwrap();
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM\0\x2a\0\0\0\x08");
        tiff.extend_from_slice(&[0, 1]);
        tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1]);
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        app1.extend_from_slice(b"SECRET-GPS-48.8N");
        let len = (app1.len() + 2) as u16;
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[test]
    fn rotates_and_strips_jpeg() {
        let input = jpeg_with_exif(40, 20, 6);
        assert!(input.windows(6).any(|w| w == b"SECRET"));
        let out = sanitize_image(&input, Options::default()).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (20, 40));
        assert!(!out.windows(6).any(|w| w == b"SECRET"));
        assert!(exif::Reader::new()
            .read_from_container(&mut Cursor::new(&out))
            .is_err());
    }

    #[test]
    fn png_roundtrip() {
        let mut png = Vec::new();
        RgbImage::new(8, 8)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let out = sanitize_image(&png, Options::default()).unwrap();
        assert_eq!(image::load_from_memory(&out).unwrap().width(), 8);
    }

    #[test]
    fn rejects_garbage() {
        assert!(sanitize_image(b"not an image", Options::default()).is_err());
    }
}
