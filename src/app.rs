use eframe::egui;
use std::{
    net::{SocketAddr, ToSocketAddrs},
    sync::mpsc,
};

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

#[cfg(target_os = "android")]
use crate::adb;

pub struct AremApp {
    address: String,
    status: String,
    model: Option<String>,
    connecting: bool,
    events: Option<mpsc::Receiver<Event>>,

    #[cfg(target_os = "android")]
    android_app: AndroidApp,
}

enum Event {
    Status(String),
    Connected(String),
    Error(String),
}

impl AremApp {
    #[cfg(not(target_os = "android"))]
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            address: "10.21.101.34:5555".to_owned(),
            status: "ADB over TCP is available on Android only.".to_owned(),
            model: None,
            connecting: false,
            events: None,
        }
    }

    #[cfg(target_os = "android")]
    pub fn new(_cc: &eframe::CreationContext<'_>, android_app: AndroidApp) -> Self {
        Self {
            address: "10.21.101.34:5555".to_owned(),
            status: "Ready.".to_owned(),
            model: None,
            connecting: false,
            events: None,
            android_app,
        }
    }

    fn poll_events(&mut self) {
        let Some(events) = &self.events else {
            return;
        };
        while let Ok(event) = events.try_recv() {
            match event {
                Event::Status(status) => self.status = status,
                Event::Connected(model) => {
                    self.model = Some(model);
                    self.status = "Connected.".to_owned();
                    self.connecting = false;
                }
                Event::Error(error) => {
                    self.status = error;
                    self.connecting = false;
                }
            }
        }
    }

    #[cfg(target_os = "android")]
    fn connect(&mut self, ctx: &egui::Context) {
        if self.connecting {
            return;
        }

        let address = match resolve(&self.address) {
            Ok(address) => address,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        let key_path = match self.android_app.internal_data_path() {
            Some(path) => path.join("adbkey"),
            None => {
                self.status = "Could not resolve the app data directory.".to_owned();
                return;
            }
        };

        let (tx, rx) = mpsc::channel();
        self.events = Some(rx);
        self.model = None;
        self.connecting = true;
        self.status = "Preparing ADB key...".to_owned();

        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let emit = |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            };

            emit(Event::Status(
                "Connecting... approve the debugging prompt on the target if shown.".to_owned(),
            ));

            let result = (|| -> Result<String, String> {
                let public_key =
                    adb::ensure_public_key(&key_path).map_err(|error| error.to_string())?;
                let mut stream = adb::open(address, &public_key, "shell:getprop ro.product.model")
                    .map_err(|error| error.to_string())?;
                let mut model = String::new();
                use std::io::Read as _;
                stream
                    .read_to_string(&mut model)
                    .map_err(|error| error.to_string())?;
                Ok(model.trim().to_owned())
            })();

            match result {
                Ok(model) => emit(Event::Connected(model)),
                Err(error) => {
                    log::error!("ADB connection failed: {error}");
                    emit(Event::Error(format!("Connection failed: {error}")));
                }
            }
        });
    }

    #[cfg(not(target_os = "android"))]
    fn connect(&mut self, _ctx: &egui::Context) {}

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
        self.poll_events();

        let safe = self.safe_area(ui);
        let mut frame = egui::Frame::central_panel(ui.style());
        frame.inner_margin = egui::Margin {
            left: frame.inner_margin.left.saturating_add(safe.left),
            right: frame.inner_margin.right.saturating_add(safe.right),
            top: frame.inner_margin.top.saturating_add(safe.top),
            bottom: frame.inner_margin.bottom.saturating_add(safe.bottom),
        };
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.heading(crate::APP_NAME);
            ui.add_space(8.0);

            egui::Grid::new("connection")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("ADB address");
                    ui.add_enabled(
                        !self.connecting,
                        egui::TextEdit::singleline(&mut self.address).desired_width(240.0),
                    );
                    ui.end_row();
                });

            ui.add_space(8.0);
            if ui
                .add_enabled(
                    !self.connecting,
                    egui::Button::new(if self.connecting {
                        "Connecting..."
                    } else {
                        "Connect"
                    }),
                )
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.connect(&ctx);
            }

            if !self.status.is_empty() {
                ui.add_space(8.0);
                ui.label(&self.status);
            }
            if let Some(model) = &self.model {
                ui.add_space(8.0);
                ui.label(format!("Device: {model}"));
            }
        });
    }
}

fn resolve(address: &str) -> Result<SocketAddr, String> {
    let address = address.trim();
    if address.is_empty() {
        return Err("Enter the target ADB address.".to_owned());
    }
    let with_port = if address
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
    {
        address.to_owned()
    } else {
        format!("{address}:5555")
    };

    with_port
        .to_socket_addrs()
        .map_err(|error| format!("Invalid address: {error}"))?
        .next()
        .ok_or_else(|| "Invalid address.".to_owned())
}
