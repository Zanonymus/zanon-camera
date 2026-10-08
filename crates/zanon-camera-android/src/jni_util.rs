//! JNI helpers: camera permission, saving to MediaStore, system (Material You) colours.

use jni::objects::{JObject, JValue};
use jni::{JNIEnv, JavaVM};

fn with_env<R>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let r = f(&mut env, &activity);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
        return None;
    }
    if let Err(e) = &r {
        eprintln!("jni error: {e}");
    }
    r.ok()
}

static ACTIVITY: std::sync::atomic::AtomicPtr<std::ffi::c_void> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

pub fn set_activity(ptr: *mut std::ffi::c_void) {
    ACTIVITY.store(ptr, std::sync::atomic::Ordering::SeqCst);
}

pub const CAMERA: &str = "android.permission.CAMERA";
pub const MICROPHONE: &str = "android.permission.RECORD_AUDIO";

pub fn has_camera_permission() -> bool {
    has_permission(CAMERA)
}

pub fn has_permission(name: &str) -> bool {
    with_env(|env, act| {
        let p = env.new_string(name)?;
        env.call_method(act, "checkSelfPermission", "(Ljava/lang/String;)I", &[JValue::Object(&p)])?.i()
    })
    .map(|r| r == 0)
    .unwrap_or(false)
}

pub fn request_camera_permission() {
    request_permission(CAMERA)
}

pub fn request_permission(name: &str) {
    with_env(|env, _app| {
        let act = unsafe { JObject::from_raw(ACTIVITY.load(std::sync::atomic::Ordering::SeqCst).cast()) };
        let act = &act;
        let arr = env.new_object_array(1, "java/lang/String", env.new_string(name)?)?;
        env.call_method(act, "requestPermissions", "([Ljava/lang/String;I)V", &[JValue::Object(&arr), JValue::Int(1)])?;
        Ok(())
    });
}

/// Inserts a JPEG into the shared gallery (DCIM/Camera). Only name, mime type and folder are given:
/// no location, no date taken.
pub fn save_jpeg(name: &str, data: &[u8]) -> Option<String> {
    with_env(|env, act| {
        let resolver = env.call_method(act, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?.l()?;
        let values = env.new_object("android/content/ContentValues", "()V", &[])?;
        for (k, v) in [("_display_name", name), ("mime_type", "image/jpeg"), ("relative_path", "DCIM/Camera")] {
            let (k, v) = (env.new_string(k)?, env.new_string(v)?);
            env.call_method(
                &values,
                "put",
                "(Ljava/lang/String;Ljava/lang/String;)V",
                &[JValue::Object(&k), JValue::Object(&v)],
            )?;
        }
        let url = env.new_string("content://media/external/images/media")?;
        let base = env
            .call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&url)])?
            .l()?;
        let uri = env
            .call_method(
                &resolver,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&base), JValue::Object(&values)],
            )?
            .l()?;
        if uri.is_null() {
            return Ok(None);
        }
        let os = env
            .call_method(&resolver, "openOutputStream", "(Landroid/net/Uri;)Ljava/io/OutputStream;", &[JValue::Object(&uri)])?
            .l()?;
        let bytes = env.byte_array_from_slice(data)?;
        env.call_method(&os, "write", "([B)V", &[JValue::Object(&bytes)])?;
        env.call_method(&os, "close", "()V", &[])?;
        let s = env.call_method(&uri, "toString", "()Ljava/lang/String;", &[])?.l()?;
        let s: String = env.get_string(&s.into())?.into();
        Ok(Some(s))
    })
    .flatten()
}

/// Opens a MediaStore item in the user's gallery app.
pub fn open_in_gallery(uri: &str, mime: &str) {
    with_env(|env, _app| {
        let act = unsafe { JObject::from_raw(ACTIVITY.load(std::sync::atomic::Ordering::SeqCst).cast()) };
        let (u, m, action) = (env.new_string(uri)?, env.new_string(mime)?, env.new_string("android.intent.action.VIEW")?);
        let uri = env
            .call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&u)])?
            .l()?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        env.call_method(
            &intent,
            "setDataAndType",
            "(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&uri), JValue::Object(&m)],
        )?;
        env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(1)])?;
        env.call_method(&act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    });
}

pub fn is_dark() -> bool {
    with_env(|env, act| {
        let res = env.call_method(act, "getResources", "()Landroid/content/res/Resources;", &[])?.l()?;
        let cfg = env.call_method(&res, "getConfiguration", "()Landroid/content/res/Configuration;", &[])?.l()?;
        let mode = env.get_field(&cfg, "uiMode", "I")?.i()?;
        Ok(mode & 0x30 == 0x20)
    })
    .unwrap_or(true)
}

/// System palette colour as 0xRRGGBB, e.g. `system_accent1_600`.
pub fn system_color(name: &str) -> Option<u32> {
    with_env(|env, act| {
        let res = env.call_method(act, "getResources", "()Landroid/content/res/Resources;", &[])?.l()?;
        let (n, t, p) = (env.new_string(name)?, env.new_string("color")?, env.new_string("android")?);
        let id = env
            .call_method(
                &res,
                "getIdentifier",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I",
                &[JValue::Object(&n), JValue::Object(&t), JValue::Object(&p)],
            )?
            .i()?;
        if id == 0 {
            return Ok(None);
        }
        let c = env
            .call_method(&res, "getColor", "(ILandroid/content/res/Resources$Theme;)I", &[JValue::Int(id), JValue::Object(&JObject::null())])?
            .i()?;
        Ok(Some(c as u32 & 0x00ff_ffff))
    })
    .flatten()
}

/// Creates a pending DCIM/Camera video entry (name, mime type and folder only) and returns its URI
/// and a writable file descriptor that the caller owns.
pub fn create_video(name: &str) -> Option<(String, i32)> {
    with_env(|env, act| {
        let resolver = env.call_method(act, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?.l()?;
        let values = env.new_object("android/content/ContentValues", "()V", &[])?;
        for (k, v) in [("_display_name", name), ("mime_type", "video/mp4"), ("relative_path", "DCIM/Camera")] {
            let (k, v) = (env.new_string(k)?, env.new_string(v)?);
            env.call_method(&values, "put", "(Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&k), JValue::Object(&v)])?;
        }
        let k = env.new_string("is_pending")?;
        let one = env.call_static_method("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;", &[JValue::Int(1)])?.l()?;
        env.call_method(&values, "put", "(Ljava/lang/String;Ljava/lang/Integer;)V", &[JValue::Object(&k), JValue::Object(&one)])?;
        let url = env.new_string("content://media/external/video/media")?;
        let base = env.call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&url)])?.l()?;
        let uri = env
            .call_method(
                &resolver,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&base), JValue::Object(&values)],
            )?
            .l()?;
        if uri.is_null() {
            return Ok(None);
        }
        let mode = env.new_string("rw")?;
        let pfd = env
            .call_method(
                &resolver,
                "openFileDescriptor",
                "(Landroid/net/Uri;Ljava/lang/String;)Landroid/os/ParcelFileDescriptor;",
                &[JValue::Object(&uri), JValue::Object(&mode)],
            )?
            .l()?;
        let fd = env.call_method(&pfd, "detachFd", "()I", &[])?.i()?;
        let s = env.call_method(&uri, "toString", "()Ljava/lang/String;", &[])?.l()?;
        let s: String = env.get_string(&s.into())?.into();
        Ok(Some((s, fd)))
    })
    .flatten()
}

/// Makes a pending entry visible in the gallery, or deletes it if the recording failed.
pub fn finish_video(uri: &str, keep: bool) {
    with_env(|env, act| {
        let resolver = env.call_method(act, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?.l()?;
        let u = env.new_string(uri)?;
        let uri = env.call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&u)])?.l()?;
        let null = JObject::null();
        if keep {
            let values = env.new_object("android/content/ContentValues", "()V", &[])?;
            let k = env.new_string("is_pending")?;
            let zero = env.call_static_method("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;", &[JValue::Int(0)])?.l()?;
            env.call_method(&values, "put", "(Ljava/lang/String;Ljava/lang/Integer;)V", &[JValue::Object(&k), JValue::Object(&zero)])?;
            env.call_method(
                &resolver,
                "update",
                "(Landroid/net/Uri;Landroid/content/ContentValues;Ljava/lang/String;[Ljava/lang/String;)I",
                &[JValue::Object(&uri), JValue::Object(&values), JValue::Object(&null), JValue::Object(&null)],
            )?;
        } else {
            env.call_method(
                &resolver,
                "delete",
                "(Landroid/net/Uri;Ljava/lang/String;[Ljava/lang/String;)I",
                &[JValue::Object(&uri), JValue::Object(&null), JValue::Object(&null)],
            )?;
        }
        Ok(())
    });
}

#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
}

/// Writes a line to logcat under the tag `zanon`.
pub fn log(msg: &str) {
    if let Ok(text) = std::ffi::CString::new(msg) {
        unsafe { __android_log_write(4, c"zanon".as_ptr(), text.as_ptr()) };
    }
}
