//! AREM, a native Android remote built with egui.

#[cfg(target_os = "android")]
mod adb;
mod app;

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

pub use app::AremApp;

/// Product name, used for the window title and Android log tag.
pub const APP_NAME: &str = "AREM";

fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    }
}

/// Runs the application in a desktop window.
#[cfg(not(target_os = "android"))]
pub fn run_desktop() -> eframe::Result {
    eframe::run_native(
        APP_NAME,
        native_options(),
        Box::new(|cc| Ok(Box::new(AremApp::new(cc)))),
    )
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag(APP_NAME),
    );

    let options = eframe::NativeOptions {
        android_app: Some(app.clone()),
        ..native_options()
    };

    let result = eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(AremApp::new(cc, app.clone())))),
    );

    if let Err(err) = result {
        log::error!("{APP_NAME} failed to start: {err}");
    }
}
