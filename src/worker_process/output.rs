//! Output truncation and error rendering helpers for worker processes.

use std::{borrow::Cow, fmt::Write as FmtWrite, process::Output};

use color_eyre::eyre::eyre;

use crate::error::BootstrapError;

pub(super) const OUTPUT_CHAR_LIMIT: usize = 2_048;
pub(super) const TRUNCATION_SUFFIX: &str = "… [truncated]";

pub(super) fn render_failure(context: &str, output: &Output) -> BootstrapError {
    let stdout = truncate_output(String::from_utf8_lossy(&output.stdout));
    let stderr = truncate_output(String::from_utf8_lossy(&output.stderr));
    BootstrapError::from(eyre!("{context}\nstdout: {stdout}\nstderr: {stderr}"))
}

pub(super) fn combine_errors(primary: BootstrapError, cleanup: BootstrapError) -> BootstrapError {
    let primary_report = primary.into_report();
    let cleanup_report = cleanup.into_report();
    BootstrapError::from(eyre!(
        "{primary_report}\nSecondary failure whilst removing worker payload: {cleanup_report}"
    ))
}

pub(super) fn append_error_context(
    message: &mut String,
    detail: &str,
    error: &impl std::fmt::Display,
    fallback: &str,
) {
    if FmtWrite::write_fmt(message, format_args!("; {detail}: {error}")).is_err() {
        message.push_str(fallback);
    }
}

pub(super) fn truncate_output(text: Cow<'_, str>) -> String {
    let mut out = String::with_capacity(OUTPUT_CHAR_LIMIT + TRUNCATION_SUFFIX.len());
    let mut chars = text.chars();
    for _ in 0..OUTPUT_CHAR_LIMIT {
        match chars.next() {
            Some(ch) => out.push(ch),
            None => return text.into_owned(),
        }
    }

    if chars.next().is_none() {
        return text.into_owned();
    }

    out.push_str(TRUNCATION_SUFFIX);
    out
}

#[doc(hidden)]
#[must_use]
pub(crate) fn render_failure_for_tests(context: &str, output: &Output) -> BootstrapError {
    render_failure(context, output)
}

#[cfg(test)]
mod tests {
    //! Unit tests for worker output truncation and error rendering.

    use std::process::Output;

    use color_eyre::eyre::eyre;

    use super::*;

    #[cfg(unix)]
    fn output_with(stdout: &[u8], stderr: &[u8]) -> Output {
        use std::os::unix::process::ExitStatusExt as _;
        Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn truncate_output_returns_short_text_unchanged() {
        let text = "short output";
        assert_eq!(truncate_output(Cow::Borrowed(text)), text);
    }

    #[test]
    fn truncate_output_keeps_text_at_exact_limit() {
        let text = "x".repeat(OUTPUT_CHAR_LIMIT);
        let rendered = truncate_output(Cow::Owned(text.clone()));
        assert_eq!(rendered, text);
        assert!(!rendered.ends_with(TRUNCATION_SUFFIX));
    }

    #[test]
    fn truncate_output_appends_suffix_when_over_limit() {
        let text = "y".repeat(OUTPUT_CHAR_LIMIT + 10);
        let rendered = truncate_output(Cow::Owned(text));
        assert!(rendered.ends_with(TRUNCATION_SUFFIX));
        assert_eq!(
            rendered.chars().count(),
            OUTPUT_CHAR_LIMIT + TRUNCATION_SUFFIX.chars().count(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn render_failure_includes_context_and_streams() {
        let output = output_with(b"the-stdout", b"the-stderr");
        let err = render_failure("setup failed", &output);
        let message = err.to_string();
        assert!(
            message.contains("setup failed"),
            "context missing: {message}"
        );
        assert!(message.contains("the-stdout"), "stdout missing: {message}");
        assert!(message.contains("the-stderr"), "stderr missing: {message}");
    }

    #[test]
    fn combine_errors_reports_primary_and_cleanup() {
        let primary = BootstrapError::from(eyre!("primary boom"));
        let cleanup = BootstrapError::from(eyre!("cleanup boom"));
        let combined = combine_errors(primary, cleanup);
        let message = combined.to_string();
        assert!(
            message.contains("primary boom"),
            "primary missing: {message}"
        );
        assert!(
            message.contains("cleanup boom"),
            "cleanup missing: {message}"
        );
        assert!(
            message.contains("Secondary failure whilst removing worker payload"),
            "secondary annotation missing: {message}"
        );
    }

    #[test]
    fn append_error_context_appends_detail_and_error() {
        let mut message = String::from("base");
        append_error_context(&mut message, "extra detail", &"the-error", "; fallback");
        assert_eq!(message, "base; extra detail: the-error");
    }
}
