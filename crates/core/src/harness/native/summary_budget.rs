//! Summary budgeting for async-delegation results (spec §6.2). A worker's
//! report is capped, head/tail trimmed on line boundaries, and any overflow
//! is spilled to a file the parent can `read`-page. Ported from Hermes'
//! delegation summary budget; cap uses this codebase's 4-bytes/token estimate
//! (`accounting.rs`).

use std::path::{Path, PathBuf};

/// Static ceiling (Hermes' 24k chars) on a re-injected delegation summary.
const STATIC_CAP_CHARS: usize = 24_000;
/// Fraction of the kept budget given to the head (the rest to the tail).
const HEAD_FRACTION: f64 = 0.75;

/// cap = min(24k chars, 50% of the parent's remaining context headroom ÷ batch
/// size). Headroom is in tokens; ×4 converts to chars (the 4-bytes/token
/// estimate). `batch_size` floors at 1.
pub fn budget_cap_chars(remaining_headroom_tokens: u64, batch_size: usize) -> usize {
    let headroom_chars = remaining_headroom_tokens.saturating_mul(4);
    let share = headroom_chars / 2 / batch_size.max(1) as u64;
    STATIC_CAP_CHARS.min(share as usize)
}

pub struct BudgetedSummary {
    pub text: String,
    pub spilled_to: Option<PathBuf>,
}

/// Trim `report` to `cap_chars`, keeping a line-snapped head + tail; on
/// overflow write the full report to `spill_dir/{spill_stem}.txt` and append a
/// footer that teaches the parent to `read` it for the rest.
pub fn budget_summary(
    report: &str,
    cap_chars: usize,
    spill_dir: &Path,
    spill_stem: &str,
) -> BudgetedSummary {
    if report.chars().count() <= cap_chars {
        return BudgetedSummary {
            text: report.to_string(),
            spilled_to: None,
        };
    }
    let head_budget = (cap_chars as f64 * HEAD_FRACTION) as usize;
    let tail_budget = cap_chars.saturating_sub(head_budget);
    let head = snap_head(report, head_budget);
    let tail = snap_tail(report, tail_budget);

    // Spill the full report next to the session's scratch space.
    let spill_path = spill_dir.join(format!("{spill_stem}.txt"));
    let _ = std::fs::create_dir_all(spill_dir);
    let spilled = std::fs::write(&spill_path, report).is_ok();

    let footer = if spilled {
        format!(
            "\n\n[truncated] {} chars total; full result saved to {}. \
             Use the `read` tool on that path (with offset/limit) to page through it.\n\n",
            report.chars().count(),
            spill_path.display()
        )
    } else {
        "\n\n[truncated] full result unavailable\n\n".to_string()
    };

    BudgetedSummary {
        text: format!("{head}{footer}{tail}"),
        spilled_to: spilled.then_some(spill_path),
    }
}

/// First `n` chars of `s` (UTF-8 safe).
fn take_first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Last `n` chars of `s` (UTF-8 safe).
fn take_last_chars(s: &str, n: usize) -> String {
    let total = s.chars().count();
    s.chars().skip(total.saturating_sub(n)).collect()
}

/// Largest whole-line prefix of `s` within `budget` chars. If the very first
/// line alone exceeds `budget` (no line boundary to snap to), hard-cut it at a
/// char boundary so the result stays within `budget`.
fn snap_head(s: &str, budget: usize) -> String {
    let mut out = String::new();
    for line in s.split_inclusive('\n') {
        if out.chars().count() + line.chars().count() > budget {
            if out.is_empty() {
                return take_first_chars(line, budget);
            }
            break;
        }
        out.push_str(line);
    }
    out
}

/// Largest whole-line suffix of `s` within `budget` chars. If the very last
/// line alone exceeds `budget`, hard-cut to its last `budget` chars.
fn snap_tail(s: &str, budget: usize) -> String {
    let lines: Vec<&str> = s.split_inclusive('\n').collect();
    let mut out = String::new();
    for line in lines.iter().rev() {
        if out.chars().count() + line.chars().count() > budget {
            if out.is_empty() {
                return take_last_chars(line, budget);
            }
            break;
        }
        out.insert_str(0, line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_is_the_min_of_static_and_headroom_share() {
        // Huge headroom → static 24k cap wins.
        assert_eq!(budget_cap_chars(1_000_000, 1), 24_000);
        // Small headroom, batch of 2: 10_000 tokens * 4 / 2 / 2 = 10_000 chars.
        assert_eq!(budget_cap_chars(10_000, 2), 10_000);
        // Batch size floors at 1 (never divide by zero).
        assert_eq!(budget_cap_chars(10_000, 0), 20_000);
    }

    #[test]
    fn under_cap_passes_through_without_spilling() {
        let dir = tempfile::tempdir().unwrap();
        let out = budget_summary("short report\nwith two lines", 4096, dir.path(), "d-1");
        assert_eq!(out.text, "short report\nwith two lines");
        assert!(out.spilled_to.is_none());
    }

    #[test]
    fn over_cap_trims_head_tail_on_line_boundaries_and_spills() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let out = budget_summary(&body, 200, dir.path(), "d-2");
        // Spilled to a file whose content is the FULL report.
        let spill = out.spilled_to.expect("must spill");
        assert_eq!(std::fs::read_to_string(&spill).unwrap(), body);
        // Head keeps the first lines, tail the last; the footer names the spill.
        assert!(out.text.starts_with("line 0\n"));
        assert!(out.text.contains("line 499"));
        assert!(out.text.contains("[truncated]"));
        assert!(out.text.contains(&spill.display().to_string()));
        assert!(out.text.contains("read"), "footer teaches read-paging");
        // Trim happened on whole lines: no partial "lin" fragments in the kept body.
        for seg in out.text.split("[truncated]") {
            for line in seg.lines().filter(|l| l.starts_with("line ")) {
                assert!(
                    line.trim_end().split(' ').count() == 2,
                    "whole line: {line:?}"
                );
            }
        }
    }

    #[test]
    fn single_long_line_with_no_newlines_is_bounded_by_the_cap() {
        // A no-newline blob is the worst case for line-snapping: split_inclusive
        // yields one element, so the old code returned the whole report twice.
        let dir = tempfile::tempdir().unwrap();
        let blob = "x".repeat(5_000); // no '\n' anywhere
        let cap = 200usize;
        let out = budget_summary(&blob, cap, dir.path(), "blob");
        // The trimmed head+tail body must not exceed the cap (footer is extra).
        // A cheap proxy: the whole output is far smaller than the raw report,
        // and specifically the non-footer content is <= cap.
        assert!(
            out.text.chars().count() < blob.chars().count(),
            "trimmed output must be smaller than the raw report, got {} vs {}",
            out.text.chars().count(),
            blob.chars().count()
        );
        assert!(out.text.contains("[truncated]"));
        // Full report is preserved in the spill file.
        let spilled = out.spilled_to.expect("blob over cap must spill");
        assert_eq!(std::fs::read_to_string(&spilled).unwrap(), blob);
    }
}
