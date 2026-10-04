//! Body decoding helpers: content-encoding and text extraction for search.

use std::io::Read;

use crate::model::{Headers, header};

/// Maximum number of decoded bytes kept for indexing and analysis.
pub const MAX_TEXT: usize = 2 * 1024 * 1024;

/// Undoes `Content-Encoding` (gzip, deflate, br). Returns the input when the
/// encoding is unknown or the body is corrupt. A body that was cut at the
/// recording limit decodes as far as it goes.
pub fn decode_body(headers: &Headers, body: &[u8]) -> Vec<u8> {
    let enc = header(headers, "content-encoding").unwrap_or("").trim().to_ascii_lowercase();
    let limit = MAX_TEXT as u64 * 4;
    let out = match enc.as_str() {
        "gzip" | "x-gzip" => read_partial(flate2::read::MultiGzDecoder::new(body).take(limit)),
        "deflate" => {
            read_partial(flate2::read::ZlibDecoder::new(body).take(limit)).or_else(|| read_partial(flate2::read::DeflateDecoder::new(body).take(limit)))
        }
        "br" => read_partial(brotli::Decompressor::new(body, 4096).take(limit)),
        _ => return body.to_vec(),
    };
    out.unwrap_or_else(|| body.to_vec())
}

/// Everything a decoder produces before it ends or fails, or `None` when it
/// produces nothing usable.
fn read_partial(mut r: impl Read) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return Some(out),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return if out.is_empty() { None } else { Some(out) },
        }
    }
}

/// Whether a body is worth treating as text (for search, link extraction and display).
pub fn is_textual(headers: &Headers, body: &[u8]) -> bool {
    let ct = header(headers, "content-type").unwrap_or("").to_ascii_lowercase();
    if ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("javascript")
        || ct.contains("xml")
        || ct.contains("x-www-form-urlencoded")
        || ct.contains("graphql")
    {
        return true;
    }
    if ct.starts_with("image/") || ct.starts_with("audio/") || ct.starts_with("video/") || ct.contains("font") || ct.contains("octet-stream") {
        return false;
    }
    // Unknown type: sniff for valid UTF-8 without NUL bytes.
    let sample = &body[..body.len().min(1024)];
    utf8_prefix_ok(sample)
}

fn utf8_prefix_ok(sample: &[u8]) -> bool {
    match std::str::from_utf8(sample) {
        Ok(_) => !sample.contains(&0),
        // A multi-byte character may be cut at the sample boundary.
        Err(e) => e.error_len().is_none() && !sample.contains(&0),
    }
}

/// Decoded, size-limited text of a body, or `None` for binary content.
pub fn body_text(headers: &Headers, body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let decoded = decode_body(headers, body);
    if !is_textual(headers, &decoded) {
        return None;
    }
    let cut = &decoded[..decoded.len().min(MAX_TEXT)];
    Some(String::from_utf8_lossy(cut).into_owned())
}

pub fn headers_text(headers: &Headers) -> String {
    headers.iter().map(|(k, v)| format!("{k}: {v}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn gzip_roundtrip_and_text() {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"{\"token\":\"abc\"}").unwrap();
        let gz = enc.finish().unwrap();
        let h: Headers = vec![
            ("Content-Type".into(), "application/json".into()),
            ("Content-Encoding".into(), "gzip".into()),
        ];
        assert_eq!(body_text(&h, &gz).unwrap(), "{\"token\":\"abc\"}");
    }

    #[test]
    fn cut_compressed_bodies_decode_as_far_as_they_go() {
        let text: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(text.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let h: Headers = vec![("Content-Type".into(), "text/plain".into()), ("Content-Encoding".into(), "gzip".into())];
        let part = body_text(&h, &gz[..gz.len() / 2]).unwrap();
        assert!(part.starts_with("line 0\nline 1\n") && part.len() < text.len(), "{}", part.len());
        let mut enc = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
        enc.write_all(text.as_bytes()).unwrap();
        let br = enc.into_inner();
        let h: Headers = vec![("Content-Type".into(), "text/plain".into()), ("Content-Encoding".into(), "br".into())];
        assert!(body_text(&h, &br[..br.len() / 2]).unwrap().starts_with("line 0\n"));
    }

    #[test]
    fn binary_is_not_text() {
        let h: Headers = vec![("Content-Type".into(), "image/png".into())];
        assert!(body_text(&h, b"\x89PNG\r\n").is_none());
        let unknown: Headers = vec![];
        assert!(body_text(&unknown, b"\x00\x01\x02").is_none());
        assert_eq!(body_text(&unknown, b"plain words").unwrap(), "plain words");
    }
}
