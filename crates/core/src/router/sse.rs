//! Minimal incremental Server-Sent-Events parser for upstream responses.

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buf: String,
    cur_event: Option<String>,
    cur_data: Vec<String>,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        let mut out = Vec::new();
        // Process complete lines; keep the trailing partial line buffered.
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.cur_data.is_empty() {
                    out.push(SseEvent {
                        event: self.cur_event.take(),
                        data: self.cur_data.join("\n"),
                    });
                    self.cur_data.clear();
                } else {
                    self.cur_event = None;
                }
            } else if let Some(rest) = line.strip_prefix("event:") {
                self.cur_event = Some(rest.trim_start().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.cur_data.push(rest.trim_start().to_string());
            } // else: comment (":") or unknown field — ignore.
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_split_across_chunks() {
        let mut p = SseParser::default();
        let mut got = p.feed(b"event: message_start\ndata: {\"a\":");
        assert!(got.is_empty());
        got.extend(p.feed(b"1}\n\ndata: {\"b\":2}\n\n"));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].event.as_deref(), Some("message_start"));
        assert_eq!(got[0].data, "{\"a\":1}");
        assert_eq!(got[1].event, None);
        assert_eq!(got[1].data, "{\"b\":2}");
    }

    #[test]
    fn ignores_comments_and_done_is_passed_through() {
        let mut p = SseParser::default();
        let got = p.feed(b": keepalive\n\ndata: [DONE]\n\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, "[DONE]");
    }

    #[test]
    fn crlf_lines_are_handled() {
        let mut p = SseParser::default();
        let got = p.feed(b"data: {\"x\":1}\r\n\r\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, "{\"x\":1}");
    }
}
