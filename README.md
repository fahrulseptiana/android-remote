# AREM

A native Android remote built with Rust, [egui], and [eframe]. The app itself
contains no Java or Kotlin sources: Android starts the Rust `cdylib` through the
framework `NativeActivity`.

- Package ID: `id.fahrul.remote`
- App name: AREM
- Min SDK: 24
- Target / compile SDK: 36
- ABIs: `arm64-v8a`, `x86_64`

## Requirements

- Android SDK with platform 36, build-tools, and NDK `30.0.16248370`
- Rust 1.95 or newer
- Rust targets: `aarch64-linux-android`, `x86_64-linux-android`
- [`cargo-ndk`] 4.x
- JDK 17 or newer
- `ANDROID_HOME` set, or `sdk.dir` in `android/local.properties`

```sh
git submodule update --init --recursive
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
```

## Build

```sh
cd android
./gradlew :app:assembleDebug
```

The APK is written to
`android/app/build/outputs/apk/debug/app-debug.apk`. Gradle's `cargoBuild` task
compiles the Rust library with `cargo-ndk` and packages the resulting `.so`
files as JNI libraries.

## Install and run

```sh
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n id.fahrul.remote/android.app.NativeActivity
```

Follow Rust logs with:

```sh
adb logcat -s AREM:V
```

## Usage

1. Enable ADB over TCP on the target (for example `adb tcpip 5555`) and make
   sure the tablet can reach the target's IP.
2. Enter the target address in AREM and tap **Connect**.
3. On the first connection, approve the debugging prompt on the target and check
   **Always allow from this computer**. AREM uses a single multiplexed ADB
   connection, so it asks at most once per connection.
4. The target screen fills the app. Tap or drag to control it. The overlay
   provides **Back**, **Recents**, and **Disconnect**.

Video is capped at 960 px / 30 fps for smooth decoding on mobile hardware.
Only ADB over TCP/IP is supported; USB/OTG is intentionally out of scope.

## Desktop preview

The UI can also run in a native window while developing:

```sh
cargo run
```

## Third-party components

- [`third_party/scrcpy`](third_party/scrcpy) — scrcpy v5.0.1 (Apache-2.0).
  Provides the device-side `scrcpy-server` and the protocol reference.
- [`third_party/adb_client`](third_party/adb_client) - adb_client v3.2.3 (MIT).
  Kept as a protocol reference. Its public API does not expose raw
  `localabstract:` services, so AREM uses a small first-party multiplexed ADB
  client to keep all scrcpy streams on one authorized connection.

## Layout

```text
src/app.rs        connection UI, fullscreen remote, touch mapping
src/adb.rs        multiplexed ADB-over-TCP client and ADB key handling
src/decoder.rs    MediaCodec H.264 decoder to RGBA
src/session.rs    scrcpy-server setup, video/control streams
src/lib.rs        Android entry point and safe-area handling
src/main.rs       desktop preview entry point
android/          Gradle project, manifest, and resources
third_party/      pinned scrcpy and adb_client submodules
```

[egui]: https://github.com/emilk/egui
[eframe]: https://github.com/emilk/egui/tree/main/crates/eframe
[`cargo-ndk`]: https://github.com/bbqsrc/cargo-ndk
