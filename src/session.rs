//! Connects to a target, starts scrcpy-server, and streams its video/control.

use crate::{adb, decoder::Decoder};
use eframe::egui;
use std::{
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SCRCPY_SERVER_VERSION: &str = "5.0.1";
const REMOTE_JAR: &str = "/data/local/tmp/scrcpy-server.jar";
const MAX_SIZE: u32 = 960;
const MAX_FPS: u32 = 30;

#[derive(Debug)]
pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

pub enum SessionEvent {
    Status(String),
    Started {
        model: String,
        width: usize,
        height: usize,
    },
    Error(String),
}

pub enum ControlEvent {
    Touch { action: u8, x: u16, y: u16 },
    Key { action: u8, keycode: i32 },
}

pub struct Session {
    pub events: mpsc::Receiver<SessionEvent>,
    pub frames: mpsc::Receiver<VideoFrame>,
    control: mpsc::Sender<ControlEvent>,
    stop: Arc<AtomicBool>,
    auth_socket: Arc<Mutex<Option<std::net::TcpStream>>>,
    connection: Arc<Mutex<Option<Arc<adb::AdbConnection>>>>,
}

impl Session {
    pub fn start(address: SocketAddr, key_path: PathBuf, jar: Vec<u8>, ctx: egui::Context) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(1);
        let (control_tx, control_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let auth_socket = Arc::new(Mutex::new(None));
        let connection = Arc::new(Mutex::new(None));

        let worker_stop = Arc::clone(&stop);
        let worker_auth_socket = Arc::clone(&auth_socket);
        let worker_connection = Arc::clone(&connection);
        thread::spawn(move || {
            let result = run(
                address,
                &key_path,
                jar,
                &event_tx,
                &frame_tx,
                control_rx,
                &worker_stop,
                &worker_auth_socket,
                &worker_connection,
                &ctx,
            );
            if let Err(error) = result {
                log::error!("session failed: {error}");
                worker_stop.store(true, Ordering::Relaxed);
                if let Some(connection) = worker_connection.lock().unwrap().take() {
                    connection.shutdown();
                }
                let _ = event_tx.send(SessionEvent::Error(error));
                ctx.request_repaint();
            }
        });

        Self {
            events: event_rx,
            frames: frame_rx,
            control: control_tx,
            stop,
            auth_socket,
            connection,
        }
    }

    /// Send a touch action; coordinates are fixed-point 0..=65535.
    pub fn touch(&self, action: u8, x: u16, y: u16) {
        let _ = self.control.send(ControlEvent::Touch { action, x, y });
    }

    /// Send a key action (0 = down, 1 = up).
    pub fn key(&self, action: u8, keycode: i32) {
        let _ = self.control.send(ControlEvent::Key { action, keycode });
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(socket) = self.auth_socket.lock().unwrap().take() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        if let Some(connection) = self.connection.lock().unwrap().take() {
            connection.shutdown();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    address: SocketAddr,
    key_path: &Path,
    jar: Vec<u8>,
    events: &mpsc::Sender<SessionEvent>,
    frames: &mpsc::SyncSender<VideoFrame>,
    control_rx: mpsc::Receiver<ControlEvent>,
    stop: &Arc<AtomicBool>,
    auth_socket: &Arc<Mutex<Option<std::net::TcpStream>>>,
    connection_slot: &Arc<Mutex<Option<Arc<adb::AdbConnection>>>>,
    ctx: &egui::Context,
) -> Result<(), String> {
    status(
        events,
        ctx,
        "Connecting... approve the target prompt and check \"Always allow\".",
    );

    let key = adb::ensure_key(key_path).map_err(|error| error.to_string())?;
    let connection = adb::AdbConnection::connect(address, &key, auth_socket, stop)
        .map_err(|error| format!("ADB connection failed: {error}"))?;

    {
        let mut slot = connection_slot.lock().unwrap();
        if stop.load(Ordering::Relaxed) {
            connection.shutdown();
            return Err("cancelled".to_owned());
        }
        *slot = Some(Arc::clone(&connection));
    }

    let mut probe = connection
        .open("shell:getprop ro.product.model")
        .map_err(|error| format!("ADB probe failed: {error}"))?;
    probe.set_read_timeout(Some(Duration::from_secs(10)));
    let mut model = String::new();
    probe
        .read_to_string(&mut model)
        .map_err(|error| format!("ADB probe failed: {error}"))?;
    let model = model.trim().to_owned();
    drop(probe);

    status(events, ctx, "Pushing scrcpy server...");
    adb::push(&connection, REMOTE_JAR, &jar)
        .map_err(|error| format!("scrcpy server push failed: {error}"))?;

    status(events, ctx, "Starting scrcpy server...");
    let scid = new_scid();
    let server_command = format!(
        "CLASSPATH={REMOTE_JAR} app_process / com.genymobile.scrcpy.Server \
         {SCRCPY_SERVER_VERSION} scid={scid:08x} log_level=info audio=false control=true \
         tunnel_forward=true max_size={MAX_SIZE} max_fps={MAX_FPS} video_codec=h264 \
         send_dummy_byte=true send_device_meta=true send_stream_meta=true \
         send_frame_meta=true cleanup=true power_on=false"
    );
    let server = connection
        .open(&format!("shell:{server_command}"))
        .map_err(|error| format!("scrcpy server failed to start: {error}"))?;
    spawn_shell_log(server);

    let socket_name = format!("localabstract:scrcpy_{scid:08x}");
    let deadline = Instant::now() + Duration::from_secs(10);

    status(events, ctx, "Waiting for scrcpy video...");
    let mut video = retry_open(&connection, &socket_name, deadline)
        .map_err(|error| format!("video socket failed: {error}"))?;
    let control = retry_open(&connection, &socket_name, deadline)
        .map_err(|error| format!("control socket failed: {error}"))?;

    let (width, height) = read_video_meta(&mut video)?;
    let width = width.min(u16::MAX as usize);
    let height = height.min(u16::MAX as usize);

    spawn_control(
        control,
        width as u16,
        height as u16,
        control_rx,
        Arc::clone(stop),
    );
    spawn_video(
        video,
        width,
        height,
        frames.clone(),
        events.clone(),
        ctx.clone(),
        Arc::clone(stop),
    );

    let _ = events.send(SessionEvent::Started {
        model,
        width,
        height,
    });
    ctx.request_repaint();
    Ok(())
}

fn status(events: &mpsc::Sender<SessionEvent>, ctx: &egui::Context, text: &str) {
    let _ = events.send(SessionEvent::Status(text.to_owned()));
    ctx.request_repaint();
}

fn retry_open(
    connection: &Arc<adb::AdbConnection>,
    service: &str,
    deadline: Instant,
) -> Result<adb::AdbStream, String> {
    loop {
        match connection.open(service) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(200));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn read_video_meta(video: &mut adb::AdbStream) -> Result<(usize, usize), String> {
    video.set_read_timeout(Some(Duration::from_secs(10)));

    let mut meta = [0u8; 65];
    video
        .read_exact(&mut meta)
        .map_err(|error| format!("scrcpy metadata failed: {error}"))?;

    let mut codec = [0u8; 4];
    video
        .read_exact(&mut codec)
        .map_err(|error| format!("scrcpy codec failed: {error}"))?;
    if &codec != b"h264" {
        return Err(format!(
            "unsupported scrcpy codec: {}",
            String::from_utf8_lossy(&codec)
        ));
    }

    let mut session = [0u8; 12];
    video
        .read_exact(&mut session)
        .map_err(|error| format!("scrcpy session header failed: {error}"))?;
    let flags = u32::from_be_bytes(session[0..4].try_into().unwrap());
    if flags & 0x8000_0000 == 0 {
        return Err("missing scrcpy session header".to_owned());
    }
    let width = u32::from_be_bytes(session[4..8].try_into().unwrap()) as usize;
    let height = u32::from_be_bytes(session[8..12].try_into().unwrap()) as usize;
    if width == 0 || height == 0 {
        return Err("scrcpy reported an empty screen".to_owned());
    }

    video.set_read_timeout(None);
    Ok((width, height))
}

fn spawn_control(
    mut control: adb::AdbStream,
    width: u16,
    height: u16,
    rx: mpsc::Receiver<ControlEvent>,
    stop: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(event) = rx.recv_timeout(Duration::from_millis(100)) else {
                continue;
            };
            let message = match event {
                ControlEvent::Touch { action, x, y } => touch_message(action, x, y, width, height),
                ControlEvent::Key { action, keycode } => key_message(action, keycode),
            };
            if control.write_all(&message).is_err() {
                break;
            }
        }
    });
}

fn spawn_video(
    mut video: adb::AdbStream,
    width: usize,
    height: usize,
    frames: mpsc::SyncSender<VideoFrame>,
    events: mpsc::Sender<SessionEvent>,
    ctx: egui::Context,
    stop: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let decoder = match Decoder::new(width, height) {
            Ok(decoder) => decoder,
            Err(error) => {
                let _ = events.send(SessionEvent::Error(error));
                ctx.request_repaint();
                return;
            }
        };

        let mut buffer = Vec::new();
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            let mut header = [0u8; 12];
            if video.read_exact(&mut header).is_err() {
                break;
            }
            let pts = u64::from_be_bytes(header[0..8].try_into().unwrap());
            let size = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;

            if pts & (1 << 63) != 0 {
                // Capture session metadata (rotation/new display size).
                continue;
            }
            if size > 16 * 1024 * 1024 {
                break;
            }

            buffer.resize(size, 0);
            if video.read_exact(&mut buffer).is_err() {
                break;
            }

            let config = pts & (1 << 62) != 0;
            let key_frame = pts & (1 << 61) != 0;
            match decoder.decode(&buffer, pts, config, key_frame) {
                Ok(Some(frame)) => {
                    let _ = frames.try_send(frame);
                    ctx.request_repaint();
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = events.send(SessionEvent::Error(error));
                    ctx.request_repaint();
                    break;
                }
            }
        }
    });
}

fn spawn_shell_log(mut server: adb::AdbStream) {
    thread::spawn(move || {
        let mut buffer = [0u8; 2048];
        loop {
            match server.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(len) => {
                    let text = String::from_utf8_lossy(&buffer[..len]);
                    for line in text.lines().filter(|line| !line.trim().is_empty()) {
                        log::info!("[scrcpy] {}", line.trim_end());
                    }
                }
            }
        }
    });
}

fn key_message(action: u8, keycode: i32) -> Vec<u8> {
    let mut message = Vec::with_capacity(14);
    message.push(0); // TYPE_INJECT_KEYCODE
    message.push(action);
    message.extend_from_slice(&keycode.to_be_bytes());
    message.extend_from_slice(&0i32.to_be_bytes()); // repeat
    message.extend_from_slice(&0i32.to_be_bytes()); // meta state
    message
}

fn touch_message(action: u8, x: u16, y: u16, width: u16, height: u16) -> Vec<u8> {
    let pressed = action != 1;
    let mut message = Vec::with_capacity(32);
    message.push(2); // TYPE_INJECT_TOUCH_EVENT
    message.push(action);
    message.extend_from_slice(&(-2i64).to_be_bytes()); // generic finger id
    message.extend_from_slice(&(x as i32).to_be_bytes());
    message.extend_from_slice(&(y as i32).to_be_bytes());
    message.extend_from_slice(&width.to_be_bytes());
    message.extend_from_slice(&height.to_be_bytes());
    message.extend_from_slice(&(if pressed { 0xffffu16 } else { 0 }).to_be_bytes()); // pressure
    message.extend_from_slice(&0i32.to_be_bytes()); // action button
    message.extend_from_slice(&0i32.to_be_bytes()); // buttons
    message
}

fn new_scid() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    (nanos ^ std::process::id().rotate_left(13)) & 0x7fff_ffff | 1
}
