use slint::ComponentHandle;
use zanon_camera_ui::MainWindow;

mod camera;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use slint::{Rgb8Pixel, SharedPixelBuffer};
use v4l::io::traits::CaptureStream;
use zanon_camera_core::capture::{encode_jpeg, Frame, Rotation};
use zanon_camera_core::qr;

type Latest = Arc<Mutex<Option<Frame>>>;

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

    let (flip_tx, flip_rx) = mpsc::channel();
    ui.on_flip(move || {
        let _ = flip_tx.send(());
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || capture_loop(weak, latest, flip_rx));

    ui.run()
}

fn set_status(weak: &slint::Weak<MainWindow>, msg: String) {
    eprintln!("{msg}");
    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_status(msg.into()));
}

fn capture_loop(weak: slint::Weak<MainWindow>, latest: Latest, flips: Receiver<()>) {
    let mut log = Vec::new();
    let mut usable = Vec::new();
    for path in camera::candidates() {
        match camera::try_open(&path) {
            Ok(_) => usable.push(path),
            Err(e) => log.push(e),
        }
    }
    let mut idx = 0;
    while !usable.is_empty() {
        idx %= usable.len();
        match stream(&usable[idx], &weak, &latest, &flips) {
            Ok(()) => idx += 1,
            Err(e) => {
                log.push(format!("{}: {e}", usable[idx].display()));
                usable.remove(idx);
            }
        }
    }
    let mut msg = String::from("No usable camera found.");
    if camera::candidates().is_empty() {
        msg.push_str("\nThere is no /dev/video* device.");
    }
    for l in &log {
        msg.push('\n');
        msg.push_str(l);
    }
    msg.push_str("\nIf access is denied, add your user to the \"video\" group.");
    set_status(&weak, msg);
}

fn stream(path: &std::path::Path, weak: &slint::Weak<MainWindow>, latest: &Latest, flips: &Receiver<()>) -> Result<(), String> {
    let opened = camera::try_open(path)?;
    let mut stream = camera::open_stream(&opened)?;
    while flips.try_recv().is_ok() {}
    let mut n = 0u32;
    loop {
        let (buf, meta) = stream.next().map_err(|e| format!("capture failed ({e})"))?;
        if flips.try_recv().is_ok() {
            return Ok(());
        }
        let data = &buf[..(meta.bytesused as usize).min(buf.len())];
        let Some(frame) = camera::decode(&opened, data) else { continue };
        n += 1;
        let scanned = n % 5 == 0;
        let code = if scanned {
            qr::scan_gray(frame.width as usize, frame.height as usize, &frame.gray()).into_iter().next().unwrap_or_default()
        } else {
            String::new()
        };
        let pixels = SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(&frame.rgb, frame.width, frame.height);
        *latest.lock().unwrap() = Some(frame);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_preview(slint::Image::from_rgb8(pixels));
            ui.set_status("".into());
            if scanned {
                ui.set_qr_text(code.into());
            }
        });
    }
}
