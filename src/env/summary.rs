//! Helpers for rendering bounded environment change summaries.

use std::fmt::Write;

pub(super) const MAX_ENV_CHANGES_SUMMARY_LEN: usize = 512;

pub(super) fn truncate_env_changes_summary(
    changes: &str,
    max_len: usize,
    change_count: usize,
) -> String {
    if changes.len() <= max_len {
        return changes.to_owned();
    }

    let preferred_cut = find_preferred_cut_point(changes, max_len);

    let mut truncated: String = changes
        .char_indices()
        .take_while(|(idx, _)| *idx < preferred_cut)
        .map(|(_, ch)| ch)
        .collect();
    let trimmed = truncated.trim_end();
    truncated.truncate(trimmed.len());

    // Estimate how many entries were shown to annotate how many were omitted.
    let trimmed_no_trailing_comma = truncated.strip_suffix(',').unwrap_or(truncated.as_str());
    let shown_changes = trimmed_no_trailing_comma
        .chars()
        .filter(|&c| c == ',')
        .count()
        + 1;
    append_truncation_suffix(&mut truncated, shown_changes, change_count);

    truncated
}

fn find_preferred_cut_point(changes: &str, max_len: usize) -> usize {
    let mut cut = 0usize;
    let mut last_comma_before_cut = None;
    for (idx, ch) in changes.char_indices() {
        if idx >= max_len {
            break;
        }
        cut = idx + ch.len_utf8();
        if ch == ',' {
            last_comma_before_cut = Some(cut);
        }
    }

    // Only use the last comma if it's in the latter half of max_len to avoid
    // trimming away most of the summary.
    last_comma_before_cut
        .filter(|pos| pos.saturating_mul(2) > max_len)
        .unwrap_or(cut)
}

fn append_truncation_suffix(truncated: &mut String, shown_changes: usize, change_count: usize) {
    if change_count > shown_changes {
        let remaining = change_count - shown_changes;
        if !truncated.is_empty() && !truncated.ends_with(',') {
            truncated.push_str(", ");
        }
        write_truncation_suffix(truncated, remaining);
    } else if !truncated.ends_with("...") {
        truncated.push_str("...");
    }
}

fn write_truncation_suffix(truncated: &mut String, remaining: usize) {
    // Writing to a `String` is infallible; silence the result explicitly.
    let write_result = write!(truncated, "+ {remaining} more");
    debug_assert!(
        write_result.is_ok(),
        "writing truncated summary should be infallible"
    );
}

#[cfg(test)]
mod tests {
    //! Unit tests for bounded environment change summaries.

    use super::*;

    #[test]
    fn returns_input_unchanged_when_within_limit() {
        let changes = "PG_HOST=localhost,PG_PORT=5432";
        assert_eq!(
            truncate_env_changes_summary(changes, MAX_ENV_CHANGES_SUMMARY_LEN, 2),
            changes,
        );
    }

    #[test]
    fn appends_remaining_count_when_truncated_on_comma_boundary() {
        // The last comma sits in the latter half of `max_len`, so the cut
        // point snaps back to it and the omitted entries are counted.
        let summary = truncate_env_changes_summary("aaaa,bbbb,cccc,dddd", 10, 4);
        assert!(
            summary.starts_with("aaaa,bbbb"),
            "expected retained prefix, got {summary:?}"
        );
        assert!(
            summary.contains("+ 2 more"),
            "expected omitted-count suffix, got {summary:?}"
        );
    }

    #[test]
    fn inserts_separator_before_remaining_count_when_cut_mid_entry() {
        // The only comma is in the first half of `max_len`, so the cut point
        // falls mid-entry and a ", " separator precedes the suffix.
        let summary = truncate_env_changes_summary("aa,bbbbbbbbbbbb,cc", 8, 3);
        assert!(
            summary.contains(", + 1 more"),
            "expected separated omitted-count suffix, got {summary:?}"
        );
    }

    #[test]
    fn appends_ellipsis_when_all_entries_shown_but_truncated() {
        // A single oversized entry is truncated without dropping any entry, so
        // the helper signals the cut with an ellipsis rather than a count.
        let summary = truncate_env_changes_summary(&"a".repeat(20), 10, 1);
        assert!(
            summary.ends_with("..."),
            "expected trailing ellipsis, got {summary:?}"
        );
        assert!(
            !summary.contains("more"),
            "unexpected count suffix: {summary:?}"
        );
    }

    #[test]
    fn keeps_full_cut_when_only_comma_is_in_first_half() {
        // The comma at index 2 is discarded because it is not past the midpoint
        // of `max_len`, exercising the `filter(..).unwrap_or(cut)` fallback.
        let summary = truncate_env_changes_summary("ab,cccccccccccc", 10, 2);
        assert!(
            summary.starts_with("ab,ccccccc"),
            "expected retained prefix without snapping to early comma, got {summary:?}"
        );
        assert!(
            summary.ends_with("..."),
            "expected ellipsis when all entries shown, got {summary:?}"
        );
    }

    #[test]
    fn does_not_duplicate_trailing_ellipsis() {
        // Input already ending in "..." must not gain a second ellipsis.
        let input = format!("{}...", "x".repeat(20));
        let summary = truncate_env_changes_summary(&input, 12, 1);
        assert!(
            !summary.ends_with("......"),
            "ellipsis should not be duplicated, got {summary:?}"
        );
    }
}
