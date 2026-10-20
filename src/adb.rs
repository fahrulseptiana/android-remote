//! Minimal multiplexed ADB-over-TCP client.
//!
//! One TCP connection carries every service (shell, sync, local sockets), so
//! the target only has to authorize this app's key once.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rsa::{
    BigUint, RsaPrivateKey,
    pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding},
    rand_core::OsRng,
    traits::PublicKeyParts,
};
use std::{
    collections::HashMap,
    fs, io,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

const A_CNXN: u32 = 0x4e58_4e43;
const A_AUTH: u32 = 0x4854_5541;
const A_OPEN: u32 = 0x4e45_504f;
const A_OKAY: u32 = 0x5941_4b4f;
const A_CLSE: u32 = 0x4553_4c43;
const A_WRTE: u32 = 0x4554_5257;
const A_STLS: u32 = 0x534c_5453;

const AUTH_TOKEN: u32 = 1;
const AUTH_PUBLIC_KEY: u32 = 3;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const AUTH_TIMEOUT: Duration = Duration::from_secs(60);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

static NEXT_LOCAL_ID: AtomicU32 = AtomicU32::new(0x1000_0000);

struct Message {
    command: u32,
    arg0: u32,
    arg1: u32,
    payload: Vec<u8>,
}

struct StreamState {
    remote_id: Option<u32>,
    open_tx: Option<mpsc::SyncSender<io::Result<u32>>>,
    data_tx: mpsc::Sender<Vec<u8>>,
    ack_tx: mpsc::Sender<()>,
}

#[derive(Default)]
struct ConnectionState {
    streams: HashMap<u32, StreamState>,
}

/// A single authenticated ADB transport shared by all streams.
pub struct AdbConnection {
    writer: Arc<Mutex<TcpStream>>,
    state: Arc<Mutex<ConnectionState>>,
    max_payload: usize,
}

impl AdbConnection {
    pub fn connect(addr: SocketAddr, public_key: &[u8]) -> io::Result<Arc<Self>> {
        let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(AUTH_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;

        let mut payload = b"host::".to_vec();
        payload.push(0);
        send_message(&mut stream, A_CNXN, 0x0100_0000, 1_048_576, &payload)?;

        let mut key_sent = false;
        let max_payload = loop {
            let msg = read_message(&mut stream)?;
            match msg.command {
                A_AUTH if msg.arg0 == AUTH_TOKEN => {
                    if key_sent {
                        // The target is waiting for the user to approve this key.
                        // Never resend it: that would create a new prompt each time.
                        thread::sleep(Duration::from_millis(200));
                    } else {
                        send_message(&mut stream, A_AUTH, AUTH_PUBLIC_KEY, 0, public_key)?;
                        key_sent = true;
                    }
                }
                A_CNXN => break (msg.arg1.max(4096) as usize).min(1_048_576),
                A_STLS => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "target requires TLS ADB",
                    ));
                }
                _ => {
                    return Err(io::Error::other(format!(
                        "unexpected ADB command 0x{:08x} during handshake",
                        msg.command
                    )));
                }
            }
        };

        stream.set_read_timeout(None)?;
        let reader = stream.try_clone()?;
        let writer = Arc::new(Mutex::new(stream));
        let state = Arc::new(Mutex::new(ConnectionState::default()));

        let reader_state = Arc::clone(&state);
        let reader_writer = Arc::clone(&writer);
        thread::spawn(move || reader_loop(reader, reader_writer, reader_state));

        Ok(Arc::new(Self {
            writer,
            state,
            max_payload,
        }))
    }

    pub fn open(self: &Arc<Self>, service: &str) -> io::Result<AdbStream> {
        let local_id = NEXT_LOCAL_ID.fetch_add(1, Ordering::Relaxed);
        let (data_tx, data_rx) = mpsc::channel();
        let (ack_tx, ack_rx) = mpsc::channel();
        let (open_tx, open_rx) = mpsc::sync_channel(1);

        self.state.lock().unwrap().streams.insert(
            local_id,
            StreamState {
                remote_id: None,
                open_tx: Some(open_tx),
                data_tx,
                ack_tx,
            },
        );

        let mut service = service.as_bytes().to_vec();
        service.push(0);
        {
            let mut writer = self.writer.lock().unwrap();
            if let Err(error) = send_message(&mut writer, A_OPEN, local_id, 0, &service) {
                self.state.lock().unwrap().streams.remove(&local_id);
                return Err(error);
            }
        }

        match open_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(remote_id)) => Ok(AdbStream {
                connection: Arc::clone(self),
                local_id,
                remote_id,
                data_rx,
                ack_rx,
                pending: Vec::new(),
                pending_pos: 0,
                read_timeout: None,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                self.state.lock().unwrap().streams.remove(&local_id);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out opening ADB service",
                ))
            }
        }
    }

    pub fn shutdown(&self) {
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.shutdown(Shutdown::Both);
        }
        self.state.lock().unwrap().streams.clear();
    }

    fn close_stream(&self, local_id: u32, remote_id: u32) {
        self.state.lock().unwrap().streams.remove(&local_id);
        if let Ok(mut writer) = self.writer.lock() {
            let _ = send_message(&mut writer, A_CLSE, local_id, remote_id, &[]);
        }
    }
}

/// A byte stream for one ADB service.
pub struct AdbStream {
    connection: Arc<AdbConnection>,
    local_id: u32,
    remote_id: u32,
    data_rx: mpsc::Receiver<Vec<u8>>,
    ack_rx: mpsc::Receiver<()>,
    pending: Vec<u8>,
    pending_pos: usize,
    read_timeout: Option<Duration>,
}

impl AdbStream {
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.read_timeout = timeout;
    }
}

impl Read for AdbStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        while self.pending_pos >= self.pending.len() {
            let chunk = match self.read_timeout {
                Some(timeout) => match self.data_rx.recv_timeout(timeout) {
                    Ok(chunk) => chunk,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        return Err(io::Error::from(io::ErrorKind::WouldBlock));
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
                },
                None => match self.data_rx.recv() {
                    Ok(chunk) => chunk,
                    Err(_) => return Ok(0),
                },
            };
            self.pending = chunk;
            self.pending_pos = 0;
        }

        let available = &self.pending[self.pending_pos..];
        let len = available.len().min(buf.len());
        buf[..len].copy_from_slice(&available[..len]);
        self.pending_pos += len;
        Ok(len)
    }
}

impl Write for AdbStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let len = buf.len().min(self.connection.max_payload);
        {
            let mut writer = self.connection.writer.lock().unwrap();
            send_message(
                &mut writer,
                A_WRTE,
                self.local_id,
                self.remote_id,
                &buf[..len],
            )?;
        }

        match self.ack_rx.recv_timeout(IO_TIMEOUT) {
            Ok(()) => Ok(len),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out writing to ADB service",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for AdbStream {
    fn drop(&mut self) {
        self.connection.close_stream(self.local_id, self.remote_id);
    }
}

fn reader_loop(
    mut reader: TcpStream,
    writer: Arc<Mutex<TcpStream>>,
    state: Arc<Mutex<ConnectionState>>,
) {
    while let Ok(message) = read_message(&mut reader) {
        let local_id = message.arg1;
        match message.command {
            A_OKAY => {
                let mut streams = state.lock().unwrap();
                if let Some(stream) = streams.streams.get_mut(&local_id) {
                    if stream.remote_id.is_none() {
                        stream.remote_id = Some(message.arg0);
                        if let Some(open_tx) = stream.open_tx.take() {
                            let _ = open_tx.send(Ok(message.arg0));
                        }
                    } else {
                        let _ = stream.ack_tx.send(());
                    }
                }
            }
            A_WRTE => {
                let remote_id = message.arg0;
                if let Ok(mut writer) = writer.lock() {
                    let _ = send_message(&mut writer, A_OKAY, local_id, remote_id, &[]);
                }
                let streams = state.lock().unwrap();
                if let Some(stream) = streams.streams.get(&local_id) {
                    let _ = stream.data_tx.send(message.payload);
                }
            }
            A_CLSE => {
                let mut streams = state.lock().unwrap();
                if let Some(stream) = streams.streams.remove(&local_id)
                    && let Some(open_tx) = stream.open_tx
                {
                    let _ = open_tx.send(Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "device refused ADB service",
                    )));
                }
            }
            _ => {}
        }
    }

    state.lock().unwrap().streams.clear();
}

/// Load the app's ADB key, generating and persisting it on first use.
pub fn ensure_public_key(path: &Path) -> io::Result<Vec<u8>> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let key = RsaPrivateKey::new(&mut OsRng, 2048).map_err(io::Error::other)?;
        let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(io::Error::other)?;
        fs::write(path, pem.as_bytes())?;
        log::info!("generated new ADB key at {}", path.display());
    }

    let pem = fs::read_to_string(path)?;
    let key = RsaPrivateKey::from_pkcs8_pem(&pem).map_err(io::Error::other)?;
    Ok(android_public_key(&key))
}

/// Push a file through the ADB sync service.
pub fn push(connection: &Arc<AdbConnection>, remote_path: &str, data: &[u8]) -> io::Result<()> {
    let mut sync = connection.open("sync:")?;
    sync.set_read_timeout(Some(IO_TIMEOUT));

    // Android 16's sync service expects the v1 "path,mode" spec form.
    let spec = format!("{remote_path},0644");
    let mut send = Vec::with_capacity(8 + spec.len());
    send.extend_from_slice(b"SEND");
    send.extend_from_slice(&(spec.len() as u32).to_le_bytes());
    send.extend_from_slice(spec.as_bytes());
    sync.write_all(&send)?;

    for chunk in data.chunks(64 * 1024) {
        let mut packet = Vec::with_capacity(8 + chunk.len());
        packet.extend_from_slice(b"DATA");
        packet.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        packet.extend_from_slice(chunk);
        sync.write_all(&packet)?;
    }

    let mut done = Vec::with_capacity(8);
    done.extend_from_slice(b"DONE");
    done.extend_from_slice(&0u32.to_le_bytes());
    sync.write_all(&done)?;
    expect_sync_okay(&mut sync)?;
    Ok(())
}

fn expect_sync_okay(stream: &mut AdbStream) -> io::Result<()> {
    let mut response = [0u8; 8];
    stream.read_exact(&mut response)?;
    if &response[0..4] == b"OKAY" {
        return Ok(());
    }
    if &response[0..4] == b"FAIL" {
        let len = u32::from_le_bytes(response[4..8].try_into().unwrap()) as usize;
        let mut message = vec![0u8; len];
        stream.read_exact(&mut message)?;
        return Err(io::Error::other(format!(
            "ADB push failed: {}",
            String::from_utf8_lossy(&message)
        )));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid ADB sync response",
    ))
}

fn read_message(stream: &mut TcpStream) -> io::Result<Message> {
    let mut header = [0u8; 24];
    stream.read_exact(&mut header)?;

    let command = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let arg0 = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let arg1 = u32::from_le_bytes(header[8..12].try_into().unwrap());
    let len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    let magic = u32::from_le_bytes(header[20..24].try_into().unwrap());

    if command ^ 0xffff_ffff != magic {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid ADB message magic",
        ));
    }
    if len > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ADB message too large",
        ));
    }

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok(Message {
        command,
        arg0,
        arg1,
        payload,
    })
}

fn send_message(
    stream: &mut TcpStream,
    command: u32,
    arg0: u32,
    arg1: u32,
    payload: &[u8],
) -> io::Result<()> {
    let mut header = [0u8; 24];
    header[0..4].copy_from_slice(&command.to_le_bytes());
    header[4..8].copy_from_slice(&arg0.to_le_bytes());
    header[8..12].copy_from_slice(&arg1.to_le_bytes());
    header[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    header[16..20].copy_from_slice(&crc32(payload).to_le_bytes());
    header[20..24].copy_from_slice(&(command ^ 0xffff_ffff).to_le_bytes());

    stream.write_all(&header)?;
    stream.write_all(payload)
}

fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = crc32_table();
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc = TABLE[((crc ^ byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffff_ffff
}

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

fn android_public_key(key: &RsaPrivateKey) -> Vec<u8> {
    const MODULUS_BYTES: usize = 256;

    let modulus = fixed_le(key.n(), MODULUS_BYTES);
    let rr = fixed_le(&((BigUint::from(1u8) << 4096) % key.n()), MODULUS_BYTES);

    let n0 = u32::from_le_bytes(modulus[0..4].try_into().unwrap());
    let mut inv = 1u32;
    for _ in 0..5 {
        inv = inv.wrapping_mul(2u32.wrapping_sub(n0.wrapping_mul(inv)));
    }
    let n0inv = inv.wrapping_neg();

    let mut exponent = 0u32;
    for byte in key.e().to_bytes_be() {
        exponent = (exponent << 8) | byte as u32;
    }

    let mut blob = Vec::with_capacity(524);
    blob.extend_from_slice(&64u32.to_le_bytes());
    blob.extend_from_slice(&n0inv.to_le_bytes());
    blob.extend_from_slice(&modulus);
    blob.extend_from_slice(&rr);
    blob.extend_from_slice(&exponent.to_le_bytes());

    format!("{} arem@android", STANDARD.encode(blob)).into_bytes()
}

fn fixed_le(value: &BigUint, len: usize) -> Vec<u8> {
    let mut bytes = value.to_bytes_le();
    bytes.resize(len, 0);
    bytes
}
