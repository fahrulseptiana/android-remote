use eframe::egui;

#[cfg(target_os = "android")]
use std::net::{SocketAddr, ToSocketAddrs};

#[cfg(target_os = "android")]
use crate::session::{Session, SessionEvent};

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

pub struct AremApp {
    address: String,
    status: String,
    device: Option<String>,
    connecting: bool,

    #[cfg(target_os = "android")]
    screen_size: (usize, usize),
    #[cfg(target_os = "android")]
    touch_active: bool,
    #[cfg(target_os = "android")]
    last_touch: Option<egui::Pos2>,
    #[cfg(target_os = "android")]
    session: Option<Session>,
    #[cfg(target_os = "android")]
    streaming: bool,
    #[cfg(target_os = "android")]
    texture: Option<egui::TextureHandle>,
    #[cfg(target_os = "android")]
    android_app: AndroidApp,
}

impl AremApp {
    #[cfg(not(target_os = "android"))]
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            address: "10.21.101.34:5555".to_owned(),
            status: "ADB over TCP is available on Android only.".to_owned(),
            device: None,
            connecting: false,
        }
    }

    #[cfg(target_os = "android")]
    pub fn new(_cc: &eframe::CreationContext<'_>, android_app: AndroidApp) -> Self {
        Self {
            address: "10.21.101.34:5555".to_owned(),
            status: "Ready.".to_owned(),
            device: None,
            connecting: false,
            screen_size: (0, 0),
            touch_active: false,
            last_touch: None,
            session: None,
            streaming: false,
            texture: None,
            android_app,
        }
    }

    #[cfg(target_os = "android")]
    fn connect(&mut self, ctx: &egui::Context) {
        if self.connecting || self.session.is_some() {
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

        let jar = match self.load_scrcpy_server() {
            Ok(jar) => jar,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        self.session = Some(Session::start(address, key_path, jar, ctx.clone()));
        self.connecting = true;
        self.device = None;
        self.status = "Connecting...".to_owned();
    }

    #[cfg(target_os = "android")]
    fn load_scrcpy_server(&self) -> Result<Vec<u8>, String> {
        let mut asset = self
            .android_app
            .asset_manager()
            .open(c"scrcpy-server.jar")
            .ok_or_else(|| "scrcpy-server.jar is missing from the APK.".to_owned())?;
        asset
            .buffer()
            .map(|buffer| buffer.to_vec())
            .map_err(|error| format!("Could not read scrcpy-server.jar: {error}"))
    }

    #[cfg(not(target_os = "android"))]
    fn connect(&mut self, _ctx: &egui::Context) {}

    #[cfg(target_os = "android")]
    fn poll_events(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else {
            return;
        };

        let mut events = Vec::new();
        while let Ok(event) = session.events.try_recv() {
            events.push(event);
        }
        let mut latest_frame = None;
        while let Ok(frame) = session.frames.try_recv() {
            latest_frame = Some(frame);
        }

        for event in events {
            match event {
                SessionEvent::Status(status) => self.status = status,
                SessionEvent::Started {
                    model,
                    width,
                    height,
                } => {
                    self.device = Some(model);
                    self.screen_size = (width, height);
                    self.streaming = true;
                    self.connecting = false;
                    self.status = "Streaming.".to_owned();
                }
                SessionEvent::Error(error) => {
                    log::error!("session error: {error}");
                    self.status = format!("Connection failed: {error}");
                    self.session = None;
                    self.streaming = false;
                    self.connecting = false;
                    self.texture = None;
                }
            }
        }

        if let Some(frame) = latest_frame {
            self.screen_size = (frame.width, frame.height);
            let image =
                egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
            match &mut self.texture {
                Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("scrcpy", image, egui::TextureOptions::LINEAR));
                }
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    fn poll_events(&mut self, _ctx: &egui::Context) {}

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

    fn show_connect(&mut self, ui: &mut egui::Ui) {
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
            ui.horizontal(|ui| {
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

                #[cfg(target_os = "android")]
                if self.session.is_some() && ui.button("Cancel").clicked() {
                    self.session = None;
                    self.connecting = false;
                    self.texture = None;
                    self.status = "Disconnected.".to_owned();
                }
            });

            if !self.status.is_empty() {
                ui.add_space(8.0);
                ui.label(&self.status);
            }
            if let Some(device) = &self.device {
                ui.add_space(8.0);
                ui.label(format!("Device: {device}"));
            }
        });
    }

    #[cfg(target_os = "android")]
    fn show_remote(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, egui::Color32::BLACK);

        let Some(texture) = &self.texture else {
            return;
        };
        let (width, height) = self.screen_size;
        if width == 0 || height == 0 {
            return;
        }

        let image_rect = fit_rect(full, width as f32 / height as f32);
        ui.painter().image(
            texture.id(),
            image_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        let safe = self.safe_area(ui);
        let controls_pos = egui::pos2(
            full.min.x + safe.left as f32 + 12.0,
            full.min.y + safe.top as f32 + 12.0,
        );
        let controls_rect = egui::Rect::from_min_size(controls_pos, egui::vec2(600.0, 48.0));

        let pointer = ui.input(|input| input.pointer.clone());
        let pointer_pos = pointer.interact_pos();
        let over_controls = pointer_pos.is_some_and(|pos| controls_rect.contains(pos));

        if pointer.any_pressed()
            && !over_controls
            && let Some(pos) = pointer_pos
            && image_rect.contains(pos)
        {
            self.touch_active = true;
            self.last_touch = Some(pos);
            self.send_touch(0, pos, image_rect);
        }

        if self.touch_active
            && let Some(pos) = pointer_pos
            && !pointer.any_pressed()
            && self.last_touch != Some(pos)
        {
            self.last_touch = Some(pos);
            self.send_touch(2, pos, image_rect);
        }

        if pointer.any_released() && self.touch_active {
            if let Some(pos) = self.last_touch.take() {
                self.send_touch(1, pos, image_rect);
            }
            self.touch_active = false;
        }

        let mut disconnect = false;
        let mut key = None;
        #[allow(deprecated)]
        let controls_id = egui::Id::new("remote_controls");
        egui::Area::new(controls_id)
            .fixed_pos(controls_pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Disconnect").clicked() {
                            disconnect = true;
                        }
                        if ui.button("Back").clicked() {
                            key = Some(4);
                        }
                        if ui.button("Recents").clicked() {
                            key = Some(187);
                        }
                        if let Some(device) = &self.device {
                            ui.label(device);
                        }
                    });
                });
            });

        if let Some(keycode) = key
            && let Some(session) = &self.session
        {
            session.key(0, keycode);
            session.key(1, keycode);
        }

        if disconnect {
            self.session = None;
            self.streaming = false;
            self.texture = None;
            self.touch_active = false;
            self.last_touch = None;
            self.status = "Disconnected.".to_owned();
        }
    }

    #[cfg(target_os = "android")]
    fn send_touch(&self, action: u8, pos: egui::Pos2, image_rect: egui::Rect) {
        if !image_rect.contains(pos) {
            return;
        }
        let (width, height) = self.screen_size;
        if width == 0 || height == 0 {
            return;
        }
        let nx = ((pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
        let ny = ((pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
        let x = (nx * (width - 1) as f32).round() as u16;
        let y = (ny * (height - 1) as f32).round() as u16;
        if let Some(session) = &self.session {
            session.touch(action, x, y);
        }
    }
}

impl eframe::App for AremApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events(ui.ctx());

        #[cfg(target_os = "android")]
        if self.streaming {
            self.show_remote(ui);
            return;
        }

        self.show_connect(ui);
    }
}

#[cfg(target_os = "android")]
fn fit_rect(outer: egui::Rect, aspect: f32) -> egui::Rect {
    let mut size = outer.size();
    if size.x / size.y > aspect {
        size.x = size.y * aspect;
    } else {
        size.y = size.x / aspect;
    }
    egui::Rect::from_center_size(outer.center(), size)
}

#[cfg(target_os = "android")]
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
