slint::include_modules!();

pub use slint::Rgb8Pixel;

pub fn rgb(hex: u32) -> slint::Color {
    slint::Color::from_rgb_u8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}
