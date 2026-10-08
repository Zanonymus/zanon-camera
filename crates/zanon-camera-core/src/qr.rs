/// Decodes every QR code found in an 8-bit grayscale frame (`width * height` bytes, row-major).
pub fn scan_gray(width: usize, height: usize, pixels: &[u8]) -> Vec<String> {
    assert_eq!(pixels.len(), width * height);
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
        pixels[y * width + x]
    });
    img.detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, text)| text))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_generated_qr() {
        let code = qrcode::QrCode::new(b"https://example.org/zanon").unwrap();
        let scale = 6;
        let quiet = 4;
        let n = code.width();
        let size = (n + quiet * 2) * scale;
        let mut px = vec![255u8; size * size];
        for y in 0..n {
            for x in 0..n {
                if code[(x, y)] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            px[((y + quiet) * scale + dy) * size + (x + quiet) * scale + dx] = 0;
                        }
                    }
                }
            }
        }
        assert_eq!(scan_gray(size, size, &px), vec!["https://example.org/zanon"]);
    }

    #[test]
    fn blank_frame_has_no_codes() {
        assert!(scan_gray(32, 32, &[255; 32 * 32]).is_empty());
    }
}
