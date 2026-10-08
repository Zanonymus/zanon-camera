#!/usr/bin/env bash
# Builds a debug-signed arm64 APK. Needs the Android SDK (build-tools, platform) and NDK + cargo-ndk.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ANDROID_HOME:?set ANDROID_HOME}" "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"
BT=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)
PLATFORM_JAR=${ANDROID_JAR:-$(ls -d "$ANDROID_HOME"/platforms/android-*/android.jar | sort -V | tail -1)}
VERSION=${1:-0.1.0}
CODE=${2:-1}
OUT=dist; WORK=target/apk
rm -rf "$WORK"; mkdir -p "$OUT" "$WORK/lib/arm64-v8a" "$WORK/res"

cargo ndk -t arm64-v8a -P 29 build --release -p zanon-camera-android
cp target/aarch64-linux-android/release/libzanon_camera.so "$WORK/lib/arm64-v8a/"

A=crates/zanon-camera-android/android
sed "s/@VERSION_CODE@/$CODE/; s/@VERSION_NAME@/$VERSION/" $A/AndroidManifest.xml > "$WORK/AndroidManifest.xml"
"$BT/aapt2" compile --dir $A/res -o "$WORK/res.zip"
"$BT/aapt2" link -o "$WORK/base.apk" -I "$PLATFORM_JAR" --manifest "$WORK/AndroidManifest.xml" \
    --min-sdk-version 29 --target-sdk-version 35 "$WORK/res.zip"
python3 - "$WORK/base.apk" "$WORK/lib/arm64-v8a/libzanon_camera.so" <<'PY'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "a") as z:   # native libs stored uncompressed, page-aligned by zipalign
    z.write(sys.argv[2], "lib/arm64-v8a/libzanon_camera.so", compress_type=zipfile.ZIP_STORED)
PY
"$BT/zipalign" -f -p 4 "$WORK/base.apk" "$WORK/aligned.apk"
KS=${KEYSTORE:-$HOME/.android/debug.keystore}
[ -f "$KS" ] || keytool -genkeypair -keystore "$KS" -storepass android -keypass android -alias androiddebugkey \
    -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
"$BT/apksigner" sign --ks "$KS" --ks-pass pass:android --key-pass pass:android --ks-key-alias androiddebugkey \
    --out "$OUT/zanon-camera-$VERSION-arm64.apk" "$WORK/aligned.apk"
"$BT/apksigner" verify "$OUT/zanon-camera-$VERSION-arm64.apk"
ls -la "$OUT/zanon-camera-$VERSION-arm64.apk"
