# AREM

A native Android remote built with Rust, [egui], and [eframe]. The app contains
no Java or Kotlin sources: Android starts the Rust `cdylib` through the
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

## Desktop preview

The UI can also run in a native window while developing:

```sh
cargo run
```

## Layout

```text
src/lib.rs        application UI, Android entry point, safe-area handling
src/main.rs       desktop preview entry point
android/          Gradle project, manifest, and resources
```

[egui]: https://github.com/emilk/egui
[eframe]: https://github.com/emilk/egui/tree/main/crates/eframe
[`cargo-ndk`]: https://github.com/bbqsrc/cargo-ndk
