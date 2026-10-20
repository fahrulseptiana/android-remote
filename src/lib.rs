//! AREM, a native Android remote built with egui.

use eframe::egui;

/// Product name, used for the window title and Android log tag.
pub const APP_NAME: &str = "AREM";

fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    }
}

fn run(options: eframe::NativeOptions) -> eframe::Result {
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(AremApp::new(cc)))),
    )
}

/// Runs the application in a desktop window.
#[cfg(not(target_os = "android"))]
pub fn run_desktop() -> eframe::Result {
    run(native_options())
}

struct AremApp;

impl AremApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self
    }
}

impl eframe::App for AremApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(APP_NAME);
            ui.label("Remote for Android.");
        });
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag(APP_NAME),
    );

    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..native_options()
    };

    if let Err(err) = run(options) {
        log::error!("{APP_NAME} failed to start: {err}");
    }
}
