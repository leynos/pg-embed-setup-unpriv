//! Tests for marking lifecycle failures that ran cached binaries.

use color_eyre::eyre::WrapErr;
use postgresql_embedded::Error as EmbeddedError;
use rstest::rstest;

use super::note_cached_binaries;
use crate::{
    ExecutionPrivileges,
    error::{BootstrapError, LifecycleTimeout},
    test_support::{bootstrap_retry::is_transient, dummy_settings},
};

/// An extraction or copy failure as the in-process lifecycle reports it.
fn io_failure() -> BootstrapError {
    BootstrapError::from(
        Err::<(), _>(EmbeddedError::IoError("No such file or directory".into()))
            .context("postgresql_embedded::setup()")
            .expect_err("the result is an error"),
    )
}

/// A slow start, as the invoker reports it.
fn timeout() -> BootstrapError {
    BootstrapError::from(color_eyre::Report::new(LifecycleTimeout {
        context: "postgresql_embedded::start()",
        seconds: 60.0,
    }))
}

/// After a cache hit an I/O failure points at the cached tree, which a retry
/// would copy again, so it stops being transient; a timeout stays transient.
#[rstest]
#[case::io_without_cache(false, io_failure(), true)]
#[case::io_after_cache_hit(true, io_failure(), false)]
#[case::timeout_after_cache_hit(true, timeout(), true)]
fn a_cache_hit_changes_only_the_io_reading(
    #[case] cache_hit: bool,
    #[case] err: BootstrapError,
    #[case] expected: bool,
) {
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let marked = note_cached_binaries(cache_hit, &bootstrap, err);
    assert_eq!(is_transient(&marked), expected, "classified {marked:?}");
}

/// The marked failure names the cache and how to clear it, and keeps its kind.
#[test]
fn a_marked_failure_says_how_to_clear_the_entry() {
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let original = io_failure();
    let kind = original.kind();
    let marked = note_cached_binaries(true, &bootstrap, original);
    let text = format!("{marked:#}");
    assert!(text.contains("shared binary cache"), "{text}");
    assert!(text.contains("remove that cache entry"), "{text}");
    assert_eq!(marked.kind(), kind);
}
