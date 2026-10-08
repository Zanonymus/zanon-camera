# Zanon Camera

Privacy-first camera app by [Zanonymus](https://github.com/Zanonymus). Rust + Slint, one codebase for Android and Linux (Wayland and X11).

Goals: metadata is never recorded (frames are encoded by us straight from the sensor, rotation baked into pixels, no EXIF/GPS/timestamps; no post-hoc stripping), QR scanning, minimal footprint.

Layout: `crates/zanon-camera-core` (shared logic), `crates/zanon-camera-desktop` (Linux shell), `zanon-camera-android` (to come).
