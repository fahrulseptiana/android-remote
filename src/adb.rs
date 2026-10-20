//! Minimal ADB-over-TCP client.
//!
//! Only what the remote frontend needs: public-key authentication, opening a
//! device service (shell or `localabstract`), and streaming bytes over it.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rsa::{
    BigUint, RsaPrivateKey,
    pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding},
    rand_core::OsRng,
    traits::PublicKeyParts,
};
use std::{
    fs, io,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    sync::atomic::{AtomicU32, Ordering},
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
const AUTH_TIMEOUT: Duration = Duration::from_secs(120);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

static NEXT_LOCAL_ID: AtomicU32 = AtomicU32::new(0x1000_0000);

struct Message {
    command: u32,
    arg0: u32,
    arg1: u32,
    payload: Vec<u8>,
}

/// An open ADB service stream.
#[derive(Debug)]
pub struct AdbStream {
    stream: TcpStream,
    local_id: u32,
    remote_id: u32,
    max_payload: usize,
    pending: Vec<u8>,
    pending_pos: usize,
}

impl AdbStream {
    /// Interrupt pending reads and writes.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Set the timeout used while waiting for read progress.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    fn send_okay(&mut self) -> io::Result<()> {
        send_message(&mut self.stream, A_OKAY, self.local_id, self.remote_id, &[])
    }
}

impl Read for AdbStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.pending_pos < self.pending.len() {
            let available = &self.pending[self.pending_pos..];
            let len = available.len().min(buf.len());
            buf[..len].copy_from_slice(&available[..len]);
            self.pending_pos += len;
            return Ok(len);
        }

        loop {
            let msg = read_message(&mut self.stream)?;
            match msg.command {
                A_WRTE => {
                    self.send_okay()?;
                    self.pending = msg.payload;
                    self.pending_pos = 0;
                    if self.pending.is_empty() {
                        return Ok(0);
                    }
                    return self.read(buf);
                }
                A_OKAY => {}
                A_CLSE => return Ok(0),
                _ => {
                    return Err(io::Error::other(format!(
                        "unexpected ADB command 0x{:08x}",
                        msg.command
                    )));
                }
            }
        }
    }
}

impl Write for AdbStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let len = buf.len().min(self.max_payload);
        send_message(
            &mut self.stream,
            A_WRTE,
            self.local_id,
            self.remote_id,
            &buf[..len],
        )?;

        loop {
            let msg = read_message(&mut self.stream)?;
            match msg.command {
                A_OKAY => return Ok(len),
                A_WRTE => {
                    self.send_okay()?;
                    self.pending.extend_from_slice(&msg.payload);
                }
                A_CLSE => return Err(io::Error::from(io::ErrorKind::BrokenPipe)),
                _ => {}
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Load the app's ADB key, generating and persisting it on first use.
///
/// Returns the Android-encoded public key blob expected by `AUTH`.
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

/// Connect to `adbd` and open a service such as `shell:...` or
/// `localabstract:...`.
pub fn open(addr: SocketAddr, public_key: &[u8], service: &str) -> io::Result<AdbStream> {
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(AUTH_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let mut payload = b"host::".to_vec();
    payload.push(0);
    send_message(&mut stream, A_CNXN, 0x0100_0000, 1_048_576, &payload)?;

    let max_payload = loop {
        let msg = read_message(&mut stream)?;
        match msg.command {
            A_AUTH if msg.arg0 == AUTH_TOKEN => {
                send_message(&mut stream, A_AUTH, AUTH_PUBLIC_KEY, 0, public_key)?;
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

    let local_id = NEXT_LOCAL_ID.fetch_add(1, Ordering::Relaxed);
    let mut service = service.as_bytes().to_vec();
    service.push(0);
    send_message(&mut stream, A_OPEN, local_id, 0, &service)?;

    let remote_id = loop {
        let msg = read_message(&mut stream)?;
        match msg.command {
            A_OKAY => break msg.arg0,
            A_CLSE => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "device refused ADB service",
                ));
            }
            _ => {}
        }
    };

    stream.set_read_timeout(None)?;
    Ok(AdbStream {
        stream,
        local_id,
        remote_id,
        max_payload,
        pending: Vec::new(),
        pending_pos: 0,
    })
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
