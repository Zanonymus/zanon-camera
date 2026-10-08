slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    ui.set_title_text(zanon_camera_core::APP_NAME.into());
    ui.run()
}
