//! WebSocket relay and capture.
//!
//! After the proxy has forwarded a handshake and both sides switched
//! protocols, [`relay`] copies bytes between the client and the server
//! exactly as they arrive, and reads a copy of each direction to record its
//! messages: fragments are reassembled, client frames unmasked, and
//! `permessage-deflate` messages decompressed. Control frames (close, ping,
//! pong) are recorded too. Each message keeps up to the body limit.

use std::sync::Arc;
use std::time::Duration;

use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::engine::Engine;
use crate::model::{Headers, WsMessage, header, header_all, now_ms};

/// How long the other direction may take to finish once one has closed.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// Whether a request asks to switch to WebSocket.
pub fn is_handshake(headers: &http::HeaderMap) -> bool {
    let has = |name: &str, token: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    };
    has("upgrade", "websocket") && has("connection", "upgrade")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToServer,
    ToClient,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::ToServer => "to_server",
            Direction::ToClient => "to_client",
        }
    }
}

/// `permessage-deflate` as the server accepted it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Deflate {
    pub on: bool,
    /// The client starts each message with a fresh compression window.
    pub client_no_context: bool,
    pub server_no_context: bool,
}

impl Deflate {
    /// Reads the extension the server agreed to in its handshake response.
    pub fn negotiated(resp_headers: &Headers) -> Self {
        for ext in header_all(resp_headers, "sec-websocket-extensions").flat_map(|v| v.split(',')) {
            let mut parts = ext.split(';').map(str::trim);
            if parts.next().is_some_and(|name| name.eq_ignore_ascii_case("permessage-deflate")) {
                let params: Vec<String> = parts.map(|p| p.to_ascii_lowercase()).collect();
                return Deflate {
                    on: true,
                    client_no_context: params.iter().any(|p| p == "client_no_context_takeover"),
                    server_no_context: params.iter().any(|p| p == "server_no_context_takeover"),
                };
            }
        }
        Deflate::default()
    }

    fn inflater(self, dir: Direction) -> Option<Inflater> {
        self.on.then(|| Inflater {
            d: flate2::Decompress::new(false),
            fresh_each_message: match dir {
                Direction::ToServer => self.client_no_context,
                Direction::ToClient => self.server_no_context,
            },
            broken: false,
        })
    }
}

struct Inflater {
    d: flate2::Decompress,
    fresh_each_message: bool,
    /// A message failed to decompress; later ones are kept as sent.
    broken: bool,
}

impl Inflater {
    /// Decompresses `input`, handing the output to `sink`. False on corrupt data.
    fn feed(&mut self, mut input: &[u8], sink: &mut dyn FnMut(&[u8])) -> bool {
        let mut out = [0u8; 16 * 1024];
        loop {
            let (in_before, out_before) = (self.d.total_in(), self.d.total_out());
            if self.d.decompress(input, &mut out, flate2::FlushDecompress::Sync).is_err() {
                return false;
            }
            let used = (self.d.total_in() - in_before) as usize;
            let made = (self.d.total_out() - out_before) as usize;
            sink(&out[..made]);
            input = &input[used..];
            // Done once the input is used up and the output had room to spare.
            if (input.is_empty() && made < out.len()) || (used == 0 && made == 0) {
                return true;
            }
        }
    }
}

/// The header of the frame being read.
#[derive(Debug)]
struct FrameHead {
    fin: bool,
    rsv1: bool,
    opcode: u8,
    mask: Option<[u8; 4]>,
    remaining: u64,
    /// Payload bytes read so far, for the mask position.
    pos: u64,
}

/// A data message being reassembled from its frames.
struct Message {
    ts: i64,
    opcode: u8,
    compressed: bool,
    data: Vec<u8>,
    size: u64,
    truncated: bool,
}

impl Message {
    fn keep(&mut self, chunk: &[u8], limit: usize) {
        self.size += chunk.len() as u64;
        let room = limit.saturating_sub(self.data.len());
        if chunk.len() > room {
            self.truncated = true;
        }
        self.data.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
}

/// Reads one direction of a WebSocket connection, frame by frame, as bytes
/// arrive in pieces of any size.
pub struct Parser {
    dir: Direction,
    limit: usize,
    head: Vec<u8>,
    frame: Option<FrameHead>,
    message: Option<Message>,
    control: Option<Message>,
    inflater: Option<Inflater>,
}

impl Parser {
    pub fn new(dir: Direction, limit: usize, deflate: Deflate) -> Self {
        Self { dir, limit, head: vec![], frame: None, message: None, control: None, inflater: deflate.inflater(dir) }
    }

    /// Reads more bytes; finished messages are added to `out`.
    pub fn feed(&mut self, mut data: &[u8], out: &mut Vec<WsMessage>) {
        while !data.is_empty() {
            let Some(frame) = self.frame.as_mut() else {
                self.head.push(data[0]);
                data = &data[1..];
                if let Some(f) = parse_head(&self.head) {
                    self.head.clear();
                    self.start_frame(f, out);
                }
                continue;
            };
            let n = (frame.remaining.min(data.len() as u64)) as usize;
            let mut chunk = data[..n].to_vec();
            if let Some(mask) = frame.mask {
                for (i, b) in chunk.iter_mut().enumerate() {
                    *b ^= mask[((frame.pos + i as u64) % 4) as usize];
                }
            }
            frame.pos += n as u64;
            frame.remaining -= n as u64;
            data = &data[n..];
            let done = frame.remaining == 0;
            self.payload(&chunk);
            if done {
                self.end_frame(out);
            }
        }
    }

    fn start_frame(&mut self, f: FrameHead, out: &mut Vec<WsMessage>) {
        let ts = now_ms();
        let blank = |opcode, compressed| Message { ts, opcode, compressed, data: vec![], size: 0, truncated: false };
        match f.opcode {
            0 => {
                if self.message.is_none() {
                    // A continuation with nothing to continue: keep it anyway.
                    self.message = Some(blank(2, false));
                }
            }
            1 | 2 => {
                let compressed = f.rsv1 && self.inflater.as_ref().is_some_and(|i| !i.broken);
                self.message = Some(blank(f.opcode, compressed));
            }
            _ => self.control = Some(blank(f.opcode, false)),
        }
        let empty = f.remaining == 0;
        self.frame = Some(f);
        if empty {
            self.end_frame(out);
        }
    }

    fn payload(&mut self, chunk: &[u8]) {
        let limit = self.limit;
        let control = self.frame.as_ref().is_some_and(|f| f.opcode >= 8);
        if control {
            if let Some(c) = self.control.as_mut() {
                c.keep(chunk, limit);
            }
            return;
        }
        let Some(m) = self.message.as_mut() else { return };
        match self.inflater.as_mut().filter(|_| m.compressed) {
            Some(inf) => {
                if !inf.feed(chunk, &mut |d| m.keep(d, limit)) {
                    inf.broken = true;
                    m.compressed = false;
                }
            }
            None => m.keep(chunk, limit),
        }
    }

    fn end_frame(&mut self, out: &mut Vec<WsMessage>) {
        let Some(f) = self.frame.take() else { return };
        if f.opcode >= 8 {
            if let Some(c) = self.control.take() {
                out.push(self.finish(c));
            }
            return;
        }
        if !f.fin {
            return;
        }
        let Some(mut m) = self.message.take() else { return };
        if m.compressed
            && let Some(inf) = self.inflater.as_mut()
        {
            let limit = self.limit;
            // Each compressed message ends with an empty block the sender leaves off.
            if !inf.feed(&[0, 0, 0xff, 0xff], &mut |d| m.keep(d, limit)) {
                inf.broken = true;
            }
            if inf.fresh_each_message {
                inf.d.reset(false);
            }
        }
        out.push(self.finish(m));
    }

    fn finish(&self, m: Message) -> WsMessage {
        let opcode = match m.opcode {
            1 => "text".to_string(),
            2 => "binary".into(),
            8 => "close".into(),
            9 => "ping".into(),
            10 => "pong".into(),
            n => format!("opcode {n}"),
        };
        WsMessage {
            id: 0,
            exchange_id: 0,
            ts: m.ts,
            direction: self.dir.as_str().into(),
            opcode,
            payload: m.data,
            size: m.size as i64,
            truncated: m.truncated,
        }
    }
}

/// A frame header, once `head` holds all of it.
fn parse_head(head: &[u8]) -> Option<FrameHead> {
    if head.len() < 2 {
        return None;
    }
    let masked = head[1] & 0x80 != 0;
    let (len_bytes, short) = match head[1] & 0x7f {
        126 => (2, None),
        127 => (8, None),
        n => (0, Some(n as u64)),
    };
    let need = 2 + len_bytes + if masked { 4 } else { 0 };
    if head.len() < need {
        return None;
    }
    let remaining = short.unwrap_or_else(|| head[2..2 + len_bytes].iter().fold(0u64, |acc, b| (acc << 8) | *b as u64));
    let mask = masked.then(|| {
        let m = &head[2 + len_bytes..need];
        [m[0], m[1], m[2], m[3]]
    });
    Some(FrameHead { fin: head[0] & 0x80 != 0, rsv1: head[0] & 0x40 != 0, opcode: head[0] & 0x0f, mask, remaining, pos: 0 })
}

/// Relays a switched connection until both sides are done, recording its
/// messages against the handshake exchange once `exchange_id` is known.
pub async fn relay(client: Upgraded, server: Upgraded, engine: Arc<Engine>, exchange_id: oneshot::Receiver<i64>, resp_headers: &Headers) {
    let limit = engine.body_limit();
    let deflate = Deflate::negotiated(resp_headers);
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(store_messages(engine, exchange_id, rx));
    let (cr, cw) = tokio::io::split(TokioIo::new(client));
    let (sr, sw) = tokio::io::split(TokioIo::new(server));
    let up = pump(cr, sw, Parser::new(Direction::ToServer, limit, deflate), tx.clone());
    let down = pump(sr, cw, Parser::new(Direction::ToClient, limit, deflate), tx);
    tokio::pin!(up, down);
    tokio::select! {
        _ = &mut up => { let _ = tokio::time::timeout(CLOSE_GRACE, down).await; }
        _ = &mut down => { let _ = tokio::time::timeout(CLOSE_GRACE, up).await; }
    }
}

/// Copies one direction and parses what passes.
async fn pump<R, W>(mut from: R, mut to: W, mut parser: Parser, tx: mpsc::UnboundedSender<WsMessage>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 16 * 1024];
    let mut found = vec![];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // Parsed before it is passed on, so a message is always recorded
        // ahead of any reply to it.
        parser.feed(&buf[..n], &mut found);
        for m in found.drain(..) {
            let _ = tx.send(m);
        }
        if to.write_all(&buf[..n]).await.is_err() || to.flush().await.is_err() {
            break;
        }
    }
    let _ = to.shutdown().await;
}

async fn store_messages(engine: Arc<Engine>, exchange_id: oneshot::Receiver<i64>, mut rx: mpsc::UnboundedReceiver<WsMessage>) {
    let Ok(id) = exchange_id.await else { return };
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while batch.len() < 500
            && let Ok(m) = rx.try_recv()
        {
            batch.push(m);
        }
        for m in &mut batch {
            m.exchange_id = id;
        }
        let engine = engine.clone();
        let stored = tokio::task::spawn_blocking(move || engine.store.insert_ws_messages(&batch)).await;
        if let Ok(Err(e)) = stored {
            tracing::error!("failed to record WebSocket messages: {e:#}");
        }
    }
}

/// The protocol a handshake asks for, as sent (normally `websocket`).
pub fn requested_protocol(req_headers: &Headers) -> String {
    header(req_headers, "upgrade").unwrap_or("websocket").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one frame as a client (masked) or server (unmasked) sends it.
    fn frame(fin: bool, rsv1: bool, opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
        let mut f = vec![(fin as u8) << 7 | (rsv1 as u8) << 6 | opcode];
        let m = if mask.is_some() { 0x80 } else { 0 };
        match payload.len() {
            n if n < 126 => f.push(m | n as u8),
            n if n < 65536 => {
                f.push(m | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(m | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        match mask {
            Some(k) => {
                f.extend_from_slice(&k);
                f.extend(payload.iter().enumerate().map(|(i, b)| b ^ k[i % 4]));
            }
            None => f.extend_from_slice(payload),
        }
        f
    }

    fn parse_in_pieces(p: &mut Parser, bytes: &[u8], piece: usize) -> Vec<WsMessage> {
        let mut out = vec![];
        for c in bytes.chunks(piece) {
            p.feed(c, &mut out);
        }
        out
    }

    #[test]
    fn reassembles_masked_fragments_around_control_frames() {
        let key = Some([1, 2, 3, 4]);
        let mut wire = frame(false, false, 1, b"hel", key);
        wire.extend(frame(true, false, 9, b"are you there", key));
        wire.extend(frame(false, false, 0, b"lo ", key));
        wire.extend(frame(true, false, 0, b"world", key));
        wire.extend(frame(true, false, 2, &[0, 1, 2], key));
        wire.extend(frame(true, false, 8, &[0x03, 0xe8, b'b', b'y', b'e'], key));
        for piece in [1, 3, 7, 1000] {
            let msgs = parse_in_pieces(&mut Parser::new(Direction::ToServer, 1024, Deflate::default()), &wire, piece);
            let got: Vec<(&str, Vec<u8>)> = msgs.iter().map(|m| (m.opcode.as_str(), m.payload.clone())).collect();
            assert_eq!(
                got,
                vec![("ping", b"are you there".to_vec()), ("text", b"hello world".to_vec()), ("binary", vec![0, 1, 2]), ("close", vec![0x03, 0xe8, b'b', b'y', b'e'])],
                "pieces of {piece}"
            );
            assert_eq!(msgs[3].text().as_deref(), Some("1000 bye"));
            assert!(msgs.iter().all(|m| m.direction == "to_server"));
        }
    }

    #[test]
    fn long_messages_are_kept_in_part() {
        let big = vec![b'x'; 70_000];
        let wire = frame(true, false, 2, &big, None);
        let msgs = parse_in_pieces(&mut Parser::new(Direction::ToClient, 100, Deflate::default()), &wire, 4096);
        assert_eq!(msgs.len(), 1);
        assert_eq!((msgs[0].payload.len(), msgs[0].size, msgs[0].truncated), (100, 70_000, true));
    }

    #[test]
    fn compressed_messages_are_decompressed_across_messages() {
        use flate2::{Compress, Compression, FlushCompress};
        // One compressor for the whole connection (context takeover), as a server does.
        let mut c = Compress::new(Compression::default(), false);
        let mut deflate = |text: &[u8]| {
            let mut out = Vec::with_capacity(text.len() + 64);
            c.compress_vec(text, &mut out, FlushCompress::Sync).unwrap();
            assert!(out.ends_with(&[0, 0, 0xff, 0xff]));
            out.truncate(out.len() - 4);
            out
        };
        let one = deflate(b"{\"event\":\"price\",\"value\":1}");
        let two = deflate(b"{\"event\":\"price\",\"value\":2}");
        let mut wire = frame(true, true, 1, &one, None);
        wire.extend(frame(false, true, 1, &two[..3], None));
        wire.extend(frame(true, false, 0, &two[3..], None));
        let ext: Headers = vec![("Sec-WebSocket-Extensions".into(), "permessage-deflate; client_no_context_takeover".into())];
        let d = Deflate::negotiated(&ext);
        assert!(d.on && d.client_no_context && !d.server_no_context);
        let msgs = parse_in_pieces(&mut Parser::new(Direction::ToClient, 1024, d), &wire, 5);
        assert_eq!(msgs[0].text().as_deref(), Some("{\"event\":\"price\",\"value\":1}"));
        assert_eq!(msgs[1].text().as_deref(), Some("{\"event\":\"price\",\"value\":2}"));
    }

    #[test]
    fn recognizes_handshakes() {
        let mut h = http::HeaderMap::new();
        h.insert("upgrade", "WebSocket".parse().unwrap());
        h.insert("connection", "keep-alive, Upgrade".parse().unwrap());
        assert!(is_handshake(&h));
        h.insert("connection", "keep-alive".parse().unwrap());
        assert!(!is_handshake(&h));
    }
}
