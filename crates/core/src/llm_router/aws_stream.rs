//! Incremental parser for the AWS binary event-stream framing used by the
//! CodeWhisperer streaming API (Kiro upstream). CRC checksums are present in
//! the wire format but intentionally NOT validated (matches 9router).
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors).
use serde_json::Value;

const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct AwsFrame {
    pub event_type: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Default)]
pub struct AwsEventStreamParser {
    buf: Vec<u8>,
}

impl AwsEventStreamParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<AwsFrame> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 12 {
                break;
            }
            let total =
                u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
            let headers_len =
                u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) as usize;
            if !(16..=MAX_FRAME).contains(&total) || headers_len + 16 > total {
                // Corrupt prelude — resync by dropping one byte.
                self.buf.remove(0);
                continue;
            }
            if self.buf.len() < total {
                break; // wait for more bytes
            }
            let frame_bytes: Vec<u8> = self.buf.drain(..total).collect();
            out.push(parse_frame(&frame_bytes, headers_len));
        }
        out
    }
}

fn parse_frame(bytes: &[u8], headers_len: usize) -> AwsFrame {
    let headers = &bytes[12..12 + headers_len];
    let event_type = read_event_type(headers);
    let payload_bytes = &bytes[12 + headers_len..bytes.len() - 4];
    let text = String::from_utf8_lossy(payload_bytes);
    let payload = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "raw": text }))
    };
    AwsFrame {
        event_type,
        payload,
    }
}

fn read_event_type(mut h: &[u8]) -> Option<String> {
    while !h.is_empty() {
        let name_len = h[0] as usize;
        if 1 + name_len + 1 > h.len() {
            return None;
        }
        let name = String::from_utf8_lossy(&h[1..1 + name_len]).to_string();
        let ty = h[1 + name_len];
        h = &h[2 + name_len..];
        if ty != 7 {
            return None; // only string headers are modeled
        }
        if h.len() < 2 {
            return None;
        }
        let vlen = u16::from_be_bytes([h[0], h[1]]) as usize;
        if 2 + vlen > h.len() {
            return None;
        }
        let value = String::from_utf8_lossy(&h[2..2 + vlen]).to_string();
        h = &h[2 + vlen..];
        if name == ":event-type" {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one AWS event-stream frame: single `:event-type` string header +
    /// JSON payload. Prelude/message CRC bytes are zeroed (parser skips them).
    fn frame(event_type: &str, payload: &str) -> Vec<u8> {
        let name = ":event-type";
        let mut headers = Vec::new();
        headers.push(name.len() as u8);
        headers.extend_from_slice(name.as_bytes());
        headers.push(7u8); // string type
        headers.extend_from_slice(&(event_type.len() as u16).to_be_bytes());
        headers.extend_from_slice(event_type.as_bytes());
        let payload_b = payload.as_bytes();
        let total = 4 + 4 + 4 + headers.len() + payload_b.len() + 4;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]); // prelude CRC (ignored)
        out.extend_from_slice(&headers);
        out.extend_from_slice(payload_b);
        out.extend_from_slice(&[0, 0, 0, 0]); // message CRC (ignored)
        out
    }

    #[test]
    fn parses_single_frame() {
        let mut p = AwsEventStreamParser::default();
        let f = p.feed(&frame("assistantResponseEvent", r#"{"content":"hi"}"#));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].event_type.as_deref(), Some("assistantResponseEvent"));
        assert_eq!(f[0].payload["content"], "hi");
    }

    #[test]
    fn reassembles_frame_split_across_chunks() {
        let bytes = frame("assistantResponseEvent", r#"{"content":"world"}"#);
        let (a, b) = bytes.split_at(9);
        let mut p = AwsEventStreamParser::default();
        assert!(p.feed(a).is_empty());
        let f = p.feed(b);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].payload["content"], "world");
    }

    #[test]
    fn parses_two_frames_in_one_chunk() {
        let mut bytes = frame("a", r#"{"n":1}"#);
        bytes.extend(frame("b", r#"{"n":2}"#));
        let mut p = AwsEventStreamParser::default();
        let f = p.feed(&bytes);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].event_type.as_deref(), Some("a"));
        assert_eq!(f[1].event_type.as_deref(), Some("b"));
    }

    #[test]
    fn empty_payload_is_null_and_nonjson_is_raw() {
        let mut p = AwsEventStreamParser::default();
        assert_eq!(
            p.feed(&frame("x", "")).remove(0).payload,
            serde_json::Value::Null
        );
        assert_eq!(
            p.feed(&frame("y", "not json")).remove(0).payload["raw"],
            "not json"
        );
    }

    #[test]
    fn corrupt_prelude_resyncs_to_next_valid_frame() {
        let mut p = AwsEventStreamParser::default();
        // A leading 12-byte prelude with total=20 but headers_len=0xFFFFFFFF, so
        // `headers_len + 16 > total` trips the corrupt guard. A valid frame
        // follows immediately: the parser must drop bytes, resync, and still
        // yield the valid frame without panicking or hanging. (If the guard were
        // removed, parse_frame would slice `bytes[12..12 + 0xFFFFFFFF]` and panic
        // with index-out-of-bounds — this test proves the resync path runs.)
        let mut bytes = vec![0u8, 0, 0, 20, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0];
        bytes.extend(frame("assistantResponseEvent", r#"{"content":"ok"}"#));
        let frames = p.feed(&bytes);
        let recovered: Vec<_> = frames
            .iter()
            .filter(|f| f.event_type.as_deref() == Some("assistantResponseEvent"))
            .collect();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].payload["content"], "ok");
    }

    #[test]
    fn oversized_total_len_does_not_hang() {
        let mut p = AwsEventStreamParser::default();
        // A leading prelude claiming total = 0x7FFFFFFF (~2 GiB, well over the
        // 16 MiB MAX_FRAME cap). The range check must fire BEFORE the
        // `buf.len() < total` wait, so a single feed containing this bogus
        // prelude plus a valid frame terminates and yields the valid frame
        // rather than blocking forever waiting for 2 GiB of bytes.
        let mut bytes = vec![0x7Fu8, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0];
        bytes.extend(frame("assistantResponseEvent", r#"{"content":"ok"}"#));
        let frames = p.feed(&bytes);
        let recovered: Vec<_> = frames
            .iter()
            .filter(|f| f.event_type.as_deref() == Some("assistantResponseEvent"))
            .collect();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].payload["content"], "ok");
    }
}
