//! Middle-truncation for tool outputs at history-ingestion time (spec §6.2).

/// 4-bytes/token heuristic used across the context manager.
const BYTES_PER_TOKEN: usize = 4;

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Keep head + tail halves of `max_bytes`, drop the middle, and prepend a
/// header telling the model exactly what it lost and how to get more.
pub fn truncate_for_context(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let original_tokens = s.len() / BYTES_PER_TOKEN;
    let original_lines = s.lines().count();
    let half = (max_bytes / 2).max(1);
    let head_end = floor_char_boundary(s, half);
    let tail_start = ceil_char_boundary(s, s.len().saturating_sub(half));
    let dropped_tokens = tail_start.saturating_sub(head_end) / BYTES_PER_TOKEN;
    format!(
        "Warning: truncated output (original ~{original_tokens} tokens, {original_lines} lines). \
         Re-run with filters (grep, head, offset/limit) to see more.\n\n{}\n\
         … [~{dropped_tokens} tokens truncated] …\n{}",
        &s[..head_end],
        &s[tail_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_passes_through() {
        assert_eq!(truncate_for_context("hello", 10_000), "hello");
    }

    #[test]
    fn oversized_output_is_middle_truncated_with_header() {
        let s = format!("{}\nMIDDLE\n{}", "a".repeat(8_000), "z".repeat(8_000));
        let out = truncate_for_context(&s, 10_000);
        assert!(out.len() < s.len());
        assert!(out.starts_with("Warning: truncated output (original ~"));
        assert!(out.contains("tokens truncated"));
        assert!(out.contains(&"a".repeat(100)), "head preserved");
        assert!(out.contains(&"z".repeat(100)), "tail preserved");
        assert!(!out.contains("MIDDLE"), "middle dropped");
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        let s = "é".repeat(20_000); // 2 bytes each
        let out = truncate_for_context(&s, 1_000);
        assert!(out.contains('é')); // no panic, valid UTF-8 by construction
    }
}
