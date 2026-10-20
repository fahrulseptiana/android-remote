//! AREM, a native Android remote built with egui.

use eframe::egui;

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

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

struct AremApp {
    #[cfg(target_os = "android")]
    android_app: AndroidApp,
}

impl AremApp {
    #[cfg(not(target_os = "android"))]
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {}
    }

    #[cfg(target_os = "android")]
    fn new(_cc: &eframe::CreationContext<'_>, android_app: AndroidApp) -> Self {
        Self { android_app }
    }

    /// Insets for system bars and display cutouts, in points.
    fn safe_area(&self, ui: &egui::Ui) -> egui::Margin {
        #[cfg(not(target_os = "android"))]
        let _ = ui;

        #[cfg(target_os = "android")]
        {
            let rect = self.android_app.content_rect();
            if rect.right <= rect.left || rect.bottom <= rect.top {
                // The activity reports an empty rect until it is first laid out.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
                return egui::Margin::ZERO;
            }

            let pixels_per_point = ui.ctx().pixels_per_point();
            let screen = ui.ctx().input(|input| input.viewport_rect().size()) * pixels_per_point;
            let to_points = |pixels: i32| {
                let points = (pixels.max(0) as f32 / pixels_per_point).round() as i32;
                points.min(i8::MAX as i32) as i8
            };

            egui::Margin {
                left: to_points(rect.left),
                right: to_points(screen.x as i32 - rect.right),
                top: to_points(rect.top),
                bottom: to_points(screen.y as i32 - rect.bottom),
            }
        }

        #[cfg(not(target_os = "android"))]
        egui::Margin::ZERO
    }
}

impl eframe::App for AremApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let safe = self.safe_area(ui);
        let mut frame = egui::Frame::central_panel(ui.style());
        frame.inner_margin = egui::Margin {
            left: frame.inner_margin.left.saturating_add(safe.left),
            right: frame.inner_margin.right.saturating_add(safe.right),
            top: frame.inner_margin.top.saturating_add(safe.top),
            bottom: frame.inner_margin.bottom.saturating_add(safe.bottom),
        };
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.heading(APP_NAME);
            ui.label("Remote for Android.");
        });
    }
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
