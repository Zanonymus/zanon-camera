mod camera;
mod jni_util;

use std::sync::mpsc;
use std::time::Duration;

use slint::{ComponentHandle, SharedPixelBuffer};
use zanon_camera_core::capture::encode_jpeg;
use zanon_camera_core::qr;
use zanon_camera_ui::{rgb, MainWindow, Theme};

enum Cmd {
    Shutter,
    Flip,
    Zoom(f32),
    Flash(i32),
}

/// Flash modes: 0 off, 1 auto, 2 on (fires with the photo), 3 torch (always lit).
const FLASH_AUTO: i32 = 1;
const FLASH_ON: i32 = 2;
const FLASH_TORCH: i32 = 3;
static LAST_URI: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn publish_caps(weak: &slint::Weak<MainWindow>, cam: &camera::Camera) {
    let (max, flash) = (cam.max_zoom, cam.has_flash);
    let _ = weak.upgrade_in_event_loop(move |ui| {
        ui.set_max_zoom(max);
        ui.set_zoom(1.0);
        ui.set_flash_available(flash);
    });
}

fn apply_material_you(ui: &MainWindow) {
    let dark = jni_util::is_dark();
    let c = |dark_name: &str, light_name: &str, fallback: u32| {
        jni_util::system_color(if dark { dark_name } else { light_name }).unwrap_or(fallback)
    };
    let t = ui.global::<Theme>();
    t.set_primary(rgb(c("system_accent1_200", "system_accent1_600", if dark { 0xd0bcff } else { 0x6750a4 })));
    t.set_on_primary(rgb(c("system_accent1_800", "system_accent1_0", if dark { 0x381e72 } else { 0xffffff })));
    t.set_primary_container(rgb(c("system_accent1_700", "system_accent1_100", if dark { 0x4f378b } else { 0xeaddff })));
    t.set_on_primary_container(rgb(c("system_accent1_100", "system_accent1_900", if dark { 0xeaddff } else { 0x21005d })));
    t.set_secondary_container(rgb(c("system_accent2_700", "system_accent2_100", if dark { 0x4a4458 } else { 0xe8def8 })));
    t.set_on_secondary_container(rgb(c("system_accent2_100", "system_accent2_900", if dark { 0xe8def8 } else { 0x1d192b })));
    t.set_surface(rgb(c("system_neutral1_900", "system_neutral1_10", if dark { 0x141218 } else { 0xfef7ff })));
    t.set_surface_container(rgb(c("system_neutral1_800", "system_neutral1_50", if dark { 0x211f26 } else { 0xf3edf7 })));
    t.set_on_surface(rgb(c("system_neutral1_100", "system_neutral1_900", if dark { 0xe6e0e9 } else { 0x1d1b20 })));
}

fn timestamp_free_name(n: u32) -> String {
    format!("IMG_{n:05}.jpg")
}

fn camera_thread(weak: slint::Weak<MainWindow>, rx: mpsc::Receiver<Cmd>) {
    if !jni_util::has_camera_permission() {
        let _ = weak.upgrade_in_event_loop(|ui| ui.set_status("Camera access is needed to take photos.\nAllow it in the permission prompt, or in Settings > Apps > Camera.".into()));
    }
    while !jni_util::has_camera_permission() {
        std::thread::sleep(Duration::from_millis(400));
    }
    let _ = weak.upgrade_in_event_loop(|ui| ui.set_status("".into()));
    let mut front = false;
    let mut cam = match camera::Camera::open(front) {
        Ok(c) => c,
        Err(e) => {
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_status(e.into()));
            return;
        }
    };
    publish_caps(&weak, &cam);
    let mut flash = 0;
    let mut luma = 128u32;
    let mut counter = 0u32;
    let mut tick = 0u32;
    loop {
        match rx.try_recv() {
            Ok(Cmd::Flip) => {
                front = !front;
                drop(cam);
                cam = match camera::Camera::open(front) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = weak.upgrade_in_event_loop(move |ui| ui.set_status(e.into()));
                        return;
                    }
                };
                publish_caps(&weak, &cam);
                cam.set_torch(flash == FLASH_TORCH);
            }
            Ok(Cmd::Zoom(z)) => {
                cam.set_zoom(z);
                let z = cam.zoom();
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_zoom(z));
            }
            Ok(Cmd::Flash(mode)) => {
                flash = mode;
                cam.set_torch(flash == FLASH_TORCH);
            }
            Ok(Cmd::Shutter) => {
                let fire = flash == FLASH_ON || (flash == FLASH_AUTO && luma < 70);
                if fire {
                    cam.set_torch(true);
                    std::thread::sleep(Duration::from_millis(450));
                }
                let mut thumb = None;
                let uri = cam.request_still().ok().and_then(|_| cam.take_still()).and_then(|f| {
                    thumb = f.thumbnail(160).rotated(cam.rotation).ok();
                    let jpeg = encode_jpeg(f, cam.rotation, 92).ok()?;
                    counter += 1;
                    jni_util::save_jpeg(&timestamp_free_name(counter), &jpeg)
                });
                if fire {
                    cam.set_torch(flash == FLASH_TORCH);
                }
                let saved = uri.is_some();
                if let Some(u) = uri {
                    *LAST_URI.lock().unwrap() = Some(u);
                }
                if let Some(t) = thumb {
                    let buf = SharedPixelBuffer::<zanon_camera_ui::Rgb8Pixel>::clone_from_slice(&t.rgb, t.width, t.height);
                    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_last_photo(slint::Image::from_rgb8(buf)));
                }
                let msg = if saved { "Saved to gallery" } else { "Could not save photo" };
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_toast(msg.into()));
                std::thread::sleep(Duration::from_millis(1400));
                let _ = weak.upgrade_in_event_loop(|ui| ui.set_toast("".into()));
            }
            Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Some(frame) = cam.preview_frame() {
            tick += 1;
            if tick % 6 == 0 {
                let g = frame.gray();
                luma = (g.iter().step_by(37).map(|&v| v as u32).sum::<u32>() * 37 / g.len().max(1) as u32).min(255);
            }
            let code = if tick % 6 == 0 {
                Some(qr::scan_gray(frame.width as usize, frame.height as usize, &frame.gray()).into_iter().next().unwrap_or_default())
            } else {
                None
            };
            let buf = SharedPixelBuffer::<zanon_camera_ui::Rgb8Pixel>::clone_from_slice(&frame.rgb, frame.width, frame.height);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_preview(slint::Image::from_rgb8(buf));
                if let Some(code) = code {
                    ui.set_qr_text(code.into());
                }
            });
        }
        std::thread::sleep(Duration::from_millis(16));
    }
}

#[no_mangle]
fn android_main(app: slint::android::AndroidApp) {
    jni_util::set_activity(app.activity_as_ptr());
    slint::android::init(app).unwrap();
    let ui = MainWindow::new().unwrap();
    apply_material_you(&ui);
    if !jni_util::has_camera_permission() {
        jni_util::request_camera_permission();
    }
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    let tx3 = tx.clone();
    ui.on_shutter(move || {
        let _ = tx.send(Cmd::Shutter);
    });
    ui.on_flip(move || {
        let _ = tx2.send(Cmd::Flip);
    });
    let (txz, txf) = (tx3.clone(), tx3);
    ui.on_set_zoom(move |z| {
        let _ = txz.send(Cmd::Zoom(z));
    });
    ui.on_set_flash(move |m| {
        let _ = txf.send(Cmd::Flash(m));
    });
    ui.on_open_gallery(|| {
        if let Some(u) = LAST_URI.lock().unwrap().clone() {
            jni_util::open_in_gallery(&u, "image/jpeg");
        }
    });
    let weak = ui.as_weak();
    ui.on_dismiss_qr(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_qr_text("".into());
        }
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || camera_thread(weak, rx));
    ui.run().unwrap();
}
