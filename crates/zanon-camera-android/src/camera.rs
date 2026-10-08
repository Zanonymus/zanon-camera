//! Camera2 through the NDK C API. Frames arrive as raw YUV from ImageReader and never pass through
//! a platform JPEG encoder, so no EXIF, GPS or timestamp is ever created.

use std::ffi::c_void;
use std::ptr;
use std::time::Duration;

use ndk::media::image_reader::{AcquireResult, Image, ImageFormat, ImageReader};
use ndk_sys::*;
use zanon_camera_core::capture::{Frame, Rotation};

const YUV_420_888: i32 = 0x23;
const MAX_STILL_PIXELS: i64 = 13_000_000;

pub struct Sizes {
    pub preview: (i32, i32),
    pub still: (i32, i32),
}

pub struct Camera {
    mgr: *mut ACameraManager,
    device: *mut ACameraDevice,
    session: *mut ACameraCaptureSession,
    container: *mut ACaptureSessionOutputContainer,
    outputs: Vec<*mut ACaptureSessionOutput>,
    preview_req: *mut ACaptureRequest,
    still_req: *mut ACaptureRequest,
    targets: Vec<*mut ACameraOutputTarget>,
    preview_reader: ImageReader,
    still_reader: ImageReader,
    pub rotation: Rotation,
    active: (i32, i32, i32, i32),
    pub max_zoom: f32,
    pub has_flash: bool,
    zoom: f32,
    torch: bool,
}

unsafe impl Send for Camera {}

extern "C" fn on_disconnected(_: *mut c_void, _: *mut ACameraDevice) {}
extern "C" fn on_error(_: *mut c_void, _: *mut ACameraDevice, _: i32) {}
extern "C" fn on_closed(_: *mut c_void, _: *mut ACameraCaptureSession) {}
extern "C" fn on_ready(_: *mut c_void, _: *mut ACameraCaptureSession) {}
extern "C" fn on_active(_: *mut c_void, _: *mut ACameraCaptureSession) {}

fn ok(status: camera_status_t, what: &str) -> Result<(), String> {
    if status.0 == 0 {
        Ok(())
    } else {
        Err(format!("{what} failed ({})", status.0))
    }
}

unsafe fn entry(meta: *const ACameraMetadata, tag: acamera_metadata_tag) -> Option<ACameraMetadata_const_entry> {
    let mut e: ACameraMetadata_const_entry = std::mem::zeroed();
    if ACameraMetadata_getConstEntry(meta, tag.0, &mut e).0 == 0 {
        Some(e)
    } else {
        None
    }
}

fn pick_sizes(configs: &[i32]) -> Option<Sizes> {
    let mut yuv: Vec<(i32, i32)> = configs
        .chunks_exact(4)
        .filter(|c| c[0] == YUV_420_888 && c[3] == 0)
        .map(|c| (c[1], c[2]))
        .collect();
    yuv.sort_by_key(|&(w, h)| std::cmp::Reverse(w as i64 * h as i64));
    let still = *yuv.iter().find(|&&(w, h)| w as i64 * h as i64 <= MAX_STILL_PIXELS)?;
    let aspect = still.0 as f32 / still.1 as f32;
    let preview = *yuv
        .iter()
        .find(|&&(w, h)| w <= 1280 && h <= 960 && ((w as f32 / h as f32) - aspect).abs() < 0.02)
        .or_else(|| yuv.iter().find(|&&(w, h)| w <= 1280 && h <= 960))?;
    Some(Sizes { preview, still })
}

impl Camera {
    pub fn open(front: bool) -> Result<Camera, String> {
        unsafe {
            let mgr = ACameraManager_create();
            let mut list: *mut ACameraIdList = ptr::null_mut();
            ok(ACameraManager_getCameraIdList(mgr, &mut list), "getCameraIdList")?;
            let want_facing = if front { 0 } else { 1 };
            let mut chosen: Option<(*const std::ffi::c_char, i32, Sizes, (i32, i32, i32, i32), f32, bool)> = None;
            for i in 0..(*list).numCameras as isize {
                let id = *(*list).cameraIds.offset(i);
                let mut meta: *mut ACameraMetadata = ptr::null_mut();
                if ACameraManager_getCameraCharacteristics(mgr, id, &mut meta).0 != 0 {
                    continue;
                }
                let facing = entry(meta, acamera_metadata_tag::ACAMERA_LENS_FACING).map(|e| *e.data.u8_ as i32);
                let orientation = entry(meta, acamera_metadata_tag::ACAMERA_SENSOR_ORIENTATION).map(|e| *e.data.i32_).unwrap_or(90);
                let sizes = entry(meta, acamera_metadata_tag::ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS).and_then(|e| {
                    pick_sizes(std::slice::from_raw_parts(e.data.i32_, e.count as usize))
                });
                let active = entry(meta, acamera_metadata_tag::ACAMERA_SENSOR_INFO_ACTIVE_ARRAY_SIZE).map(|e| {
                    let a = std::slice::from_raw_parts(e.data.i32_, 4);
                    (a[0], a[1], a[2], a[3])
                });
                let max_zoom = entry(meta, acamera_metadata_tag::ACAMERA_SCALER_AVAILABLE_MAX_DIGITAL_ZOOM)
                    .map(|e| *e.data.f)
                    .unwrap_or(1.0)
                    .clamp(1.0, 10.0);
                let has_flash = entry(meta, acamera_metadata_tag::ACAMERA_FLASH_INFO_AVAILABLE).map(|e| *e.data.u8_ == 1).unwrap_or(false);
                ACameraMetadata_free(meta);
                if facing == Some(want_facing) {
                    if let (Some(s), Some(active)) = (sizes, active) {
                        chosen = Some((id, orientation, s, active, max_zoom, has_flash));
                        break;
                    }
                }
            }
            let (id, orientation, sizes, active, max_zoom, has_flash) = chosen.ok_or("no suitable camera found")?;

            let mut device: *mut ACameraDevice = ptr::null_mut();
            let mut dcb = ACameraDevice_StateCallbacks {
                context: ptr::null_mut(),
                onDisconnected: Some(on_disconnected),
                onError: Some(on_error),
            };
            ok(ACameraManager_openCamera(mgr, id, &mut dcb, &mut device), "openCamera")?;
            ACameraManager_deleteCameraIdList(list);

            let preview_reader = ImageReader::new(sizes.preview.0, sizes.preview.1, ImageFormat::YUV_420_888, 3)
                .map_err(|e| e.to_string())?;
            let still_reader = ImageReader::new(sizes.still.0, sizes.still.1, ImageFormat::YUV_420_888, 2)
                .map_err(|e| e.to_string())?;

            let mut container: *mut ACaptureSessionOutputContainer = ptr::null_mut();
            ok(ACaptureSessionOutputContainer_create(&mut container), "outputContainer")?;
            let mut outputs = vec![];
            let mut targets = vec![];
            let mut preview_req: *mut ACaptureRequest = ptr::null_mut();
            let mut still_req: *mut ACaptureRequest = ptr::null_mut();
            ok(
                ACameraDevice_createCaptureRequest(device, ACameraDevice_request_template::TEMPLATE_PREVIEW, &mut preview_req),
                "previewRequest",
            )?;
            ok(
                ACameraDevice_createCaptureRequest(device, ACameraDevice_request_template::TEMPLATE_STILL_CAPTURE, &mut still_req),
                "stillRequest",
            )?;
            for (reader, req) in [(&preview_reader, preview_req), (&still_reader, still_req)] {
                let win = reader.window().map_err(|e| e.to_string())?;
                let win = win.ptr().as_ptr();
                let mut out: *mut ACaptureSessionOutput = ptr::null_mut();
                ok(ACaptureSessionOutput_create(win, &mut out), "sessionOutput")?;
                ok(ACaptureSessionOutputContainer_add(container, out), "outputAdd")?;
                outputs.push(out);
                let mut target: *mut ACameraOutputTarget = ptr::null_mut();
                ok(ACameraOutputTarget_create(win, &mut target), "outputTarget")?;
                ok(ACaptureRequest_addTarget(req, target), "addTarget")?;
                targets.push(target);
            }

            let mut scb = ACameraCaptureSession_stateCallbacks {
                context: ptr::null_mut(),
                onClosed: Some(on_closed),
                onReady: Some(on_ready),
                onActive: Some(on_active),
            };
            let mut session: *mut ACameraCaptureSession = ptr::null_mut();
            ok(ACameraDevice_createCaptureSession(device, container, &mut scb, &mut session), "createSession")?;
            ok(
                ACameraCaptureSession_setRepeatingRequest(session, ptr::null_mut(), 1, &mut preview_req, ptr::null_mut()),
                "setRepeatingRequest",
            )?;

            let rotation = match orientation {
                90 => Rotation::Cw90,
                180 => Rotation::Cw180,
                270 => Rotation::Cw270,
                _ => Rotation::None,
            };
            Ok(Camera {
                mgr,
                device,
                session,
                container,
                outputs,
                preview_req,
                still_req,
                targets,
                preview_reader,
                still_reader,
                rotation,
                active,
                max_zoom,
                has_flash,
                zoom: 1.0,
                torch: false,
            })
        }
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(1.0, self.max_zoom);
        self.apply();
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn set_torch(&mut self, on: bool) {
        if self.has_flash {
            self.torch = on;
            self.apply();
        }
    }

    /// Pushes zoom (sensor crop region) and torch state to both requests and restarts the repeating preview.
    fn apply(&mut self) {
        let (l, t, w, h) = self.active;
        let (cw, ch) = ((w as f32 / self.zoom) as i32, (h as f32 / self.zoom) as i32);
        let crop = [l + (w - cw) / 2, t + (h - ch) / 2, cw, ch];
        let flash: u8 = if self.torch { 2 } else { 0 };
        unsafe {
            for req in [self.preview_req, self.still_req] {
                ACaptureRequest_setEntry_i32(req, acamera_metadata_tag::ACAMERA_SCALER_CROP_REGION.0, 4, crop.as_ptr());
                ACaptureRequest_setEntry_u8(req, acamera_metadata_tag::ACAMERA_FLASH_MODE.0, 1, &flash);
            }
            let mut req = self.preview_req;
            ACameraCaptureSession_setRepeatingRequest(self.session, ptr::null_mut(), 1, &mut req, ptr::null_mut());
        }
    }

    /// Latest preview frame, downsampled to roughly 640 px wide and rotated upright.
    pub fn preview_frame(&self) -> Option<Frame> {
        let AcquireResult::Image(img) = self.preview_reader.acquire_latest_image().ok()? else { return None };
        let f = yuv_to_frame(&img, 2)?;
        f.rotated(self.rotation).ok()
    }

    pub fn request_still(&self) -> Result<(), String> {
        unsafe {
            let mut req = self.still_req;
            ok(ACameraCaptureSession_capture(self.session, ptr::null_mut(), 1, &mut req, ptr::null_mut()), "capture")
        }
    }

    /// Full-resolution frame (unrotated); wait a short while for the sensor to deliver it.
    pub fn take_still(&self) -> Option<Frame> {
        for _ in 0..100 {
            if let Ok(AcquireResult::Image(img)) = self.still_reader.acquire_latest_image() {
                return yuv_to_frame(&img, 1);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        None
    }
}

fn yuv_to_frame(img: &Image, step: u32) -> Option<Frame> {
    let (w, h) = (img.width().ok()? as u32, img.height().ok()? as u32);
    let y = img.plane_data(0).ok()?;
    let u = img.plane_data(1).ok()?;
    let v = img.plane_data(2).ok()?;
    Some(Frame::from_yuv420(
        w,
        h,
        step,
        y,
        img.plane_row_stride(0).ok()? as usize,
        u,
        v,
        img.plane_row_stride(1).ok()? as usize,
        img.plane_pixel_stride(1).ok()? as usize,
    ))
}

impl Drop for Camera {
    fn drop(&mut self) {
        unsafe {
            ACameraCaptureSession_stopRepeating(self.session);
            ACameraCaptureSession_close(self.session);
            for t in &self.targets {
                ACameraOutputTarget_free(*t);
            }
            ACaptureRequest_free(self.preview_req);
            ACaptureRequest_free(self.still_req);
            for o in &self.outputs {
                ACaptureSessionOutput_free(*o);
            }
            ACaptureSessionOutputContainer_free(self.container);
            ACameraDevice_close(self.device);
            ACameraManager_delete(self.mgr);
        }
    }
}
