//! A small Chrome DevTools Protocol client, for the browser crawl.
//!
//! Just enough WebSocket (client side, RFC 6455) to talk to a headless
//! Chromium on its loopback debugging port, and a command layer on top:
//! each command gets an id and resolves when its reply arrives. Events go
//! to a channel the caller drains in a task of its own, so an event that
//! must be answered quickly (a paused request) is never stuck behind a
//! command that is waiting for it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot};

/// The largest message accepted from the browser.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
/// How long a command may wait for its reply.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// A protocol event, with the session it belongs to (none for the browser).
#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
    pub session: Option<String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A connection to the browser. Cheap to clone; every clone shares it.
#[derive(Clone)]
pub struct Cdp {
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    next_id: Arc<AtomicU64>,
    pending: Pending,
}

impl Cdp {
    /// Connects to `ws://127.0.0.1:{port}{path}` and returns the client and
    /// its event stream. The stream ends when the browser goes away.
    pub async fn connect(port: u16, path: &str) -> Result<(Cdp, mpsc::UnboundedReceiver<Event>)> {
        let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.context("connecting to the browser's debugging port")?;
        tcp.set_nodelay(true).ok();
        let key = base64::engine::general_purpose::STANDARD.encode(random::<16>());
        let hello = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        tcp.write_all(hello.as_bytes()).await?;
        let mut buf = Vec::with_capacity(4096);
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if buf.len() > 16 * 1024 {
                bail!("the browser sent an oversized handshake");
            }
            let mut chunk = [0u8; 2048];
            let n = tcp.read(&mut chunk).await?;
            if n == 0 {
                bail!("the browser closed the connection during the handshake");
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let status = String::from_utf8_lossy(&buf[..head_end]);
        if !status.starts_with("HTTP/1.1 101") {
            bail!("the browser refused the debugging connection: {}", status.lines().next().unwrap_or(""));
        }
        let leftover = buf.split_off(head_end);
        let (read, write) = tcp.into_split();
        let cdp = Cdp { writer: Arc::new(tokio::sync::Mutex::new(write)), next_id: Arc::new(AtomicU64::new(1)), pending: Arc::default() };
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(read_loop(read, leftover, cdp.clone(), tx));
        Ok((cdp, rx))
    }

    /// Sends a command and waits for its result.
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self.write(id, session, method, params).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!("{method}: {e}")),
            Ok(Err(_)) => Err(anyhow!("{method}: the browser went away")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("{method}: no answer from the browser"))
            }
        }
    }

    /// Sends a command without waiting for its result.
    pub async fn send(&self, session: Option<&str>, method: &str, params: Value) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.write(id, session, method, params).await
    }

    async fn write(&self, id: u64, session: Option<&str>, method: &str, params: Value) -> Result<()> {
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.write_frame(OP_TEXT, msg.to_string().as_bytes()).await
    }

    async fn write_frame(&self, opcode: u8, payload: &[u8]) -> Result<()> {
        let frame = encode_frame(opcode, payload, random::<4>());
        self.writer.lock().await.write_all(&frame).await.context("writing to the browser")
    }

    fn resolve(&self, msg: Value) -> Option<Event> {
        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                let r = match msg.get("error") {
                    Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(r);
            }
            return None;
        }
        let method = msg.get("method")?.as_str()?.to_string();
        let session = msg.get("sessionId").and_then(Value::as_str).map(String::from);
        Some(Event { method, params: msg.get("params").cloned().unwrap_or(Value::Null), session })
    }
}

async fn read_loop(mut read: OwnedReadHalf, mut buf: Vec<u8>, cdp: Cdp, events: mpsc::UnboundedSender<Event>) {
    let mut message: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    'outer: loop {
        while let Some(frame) = parse_frame(&buf) {
            buf.drain(..frame.len);
            match frame.opcode {
                OP_TEXT | OP_BINARY | OP_CONT => {
                    message.extend_from_slice(&frame.payload);
                    if message.len() > MAX_MESSAGE {
                        break 'outer;
                    }
                    if frame.fin {
                        let text = std::mem::take(&mut message);
                        if let Ok(v) = serde_json::from_slice::<Value>(&text)
                            && let Some(ev) = cdp.resolve(v)
                        {
                            let _ = events.send(ev);
                        }
                    }
                }
                OP_PING => {
                    let _ = cdp.write_frame(OP_PONG, &frame.payload).await;
                }
                OP_CLOSE => break 'outer,
                _ => {}
            }
        }
        if buf.len() > MAX_MESSAGE + 16 {
            break;
        }
        match read.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    // Wake every waiter: the browser is gone.
    cdp.pending.lock().unwrap().clear();
}

/// One decoded WebSocket frame and how many bytes it took.
#[derive(Debug, PartialEq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
    pub len: usize,
}

/// Decodes one frame from the start of `buf`, or `None` when more bytes are
/// needed. Masked frames (as a client sends them) are unmasked.
pub fn parse_frame(buf: &[u8]) -> Option<Frame> {
    if buf.len() < 2 {
        return None;
    }
    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0F;
    let masked = buf[1] & 0x80 != 0;
    let (len, mut at) = match buf[1] & 0x7F {
        126 => (u16::from_be_bytes(buf.get(2..4)?.try_into().ok()?) as usize, 4),
        127 => (u64::from_be_bytes(buf.get(2..10)?.try_into().ok()?) as usize, 10),
        n => (n as usize, 2),
    };
    let mask: Option<[u8; 4]> = if masked {
        let m = buf.get(at..at + 4)?.try_into().ok()?;
        at += 4;
        Some(m)
    } else {
        None
    };
    let end = at.checked_add(len)?;
    let mut payload = buf.get(at..end)?.to_vec();
    if let Some(m) = mask {
        payload.iter_mut().enumerate().for_each(|(i, b)| *b ^= m[i % 4]);
    }
    Some(Frame { fin, opcode, payload, len: end })
}

/// Encodes a single, final, masked client frame.
pub fn encode_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => out.push(0x80 | n as u8),
        n if n <= u16::MAX as usize => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

fn random<const N: usize>() -> [u8; N] {
    use ring::rand::SecureRandom;
    let mut b = [0u8; N];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_at_every_length_class() {
        for n in [0usize, 5, 125, 126, 300, 65535, 65536, 70000] {
            let payload: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            let enc = encode_frame(OP_TEXT, &payload, [1, 2, 3, 4]);
            let f = parse_frame(&enc).expect("whole frame");
            assert!(f.fin);
            assert_eq!(f.opcode, OP_TEXT);
            assert_eq!(f.len, enc.len());
            assert_eq!(f.payload, payload);
            // A truncated frame waits for more bytes.
            assert!(parse_frame(&enc[..enc.len() - 1]).is_none());
        }
    }

    #[test]
    fn unmasked_server_frames_decode() {
        let mut buf = vec![0x81, 3, b'a', b'b', b'c'];
        buf.extend_from_slice(&[0x01, 1, b'x']);
        let f = parse_frame(&buf).unwrap();
        assert_eq!((f.fin, f.opcode, f.payload.as_slice(), f.len), (true, OP_TEXT, &b"abc"[..], 5));
        let g = parse_frame(&buf[5..]).unwrap();
        assert!(!g.fin);
    }
}
