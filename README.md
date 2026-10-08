# Zanon Camera

Privacy-first camera app by [Zanonymus](https://github.com/Zanonymus). Rust + Slint, one codebase for Android and Linux (Wayland and X11).

Goals: rotation baked into pixels, all metadata stripped (ImagePipe-style), QR scanning, minimal footprint.

Layout: `crates/zanon-camera-core` (shared logic), `crates/zanon-camera-desktop` (Linux shell), `zanon-camera-android` (to come).
