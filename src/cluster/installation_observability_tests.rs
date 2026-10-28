//! Tests for bounded installation-selection diagnostics.

use std::fs;

use postgresql_embedded::{Settings, VersionReq};
use rstest::rstest;

use super::resolve_installed_dir;
use crate::test_support::capture_debug_logs;

#[rstest]
#[case::matching_version(Some("18.6.0"), "versioned", true)]
#[case::incompatible_version(Some("18.6.1"), "not_found", false)]
#[case::lexical_fallback(Some("custom"), "unversioned", true)]
#[case::empty(None, "not_found", false)]
fn selection_reports_bounded_outcome(
    #[case] directory: Option<&str>,
    #[case] outcome: &str,
    #[case] is_selected: bool,
) {
    let temp = tempfile::tempdir().expect("create installation tree");
    if let Some(name) = directory {
        fs::create_dir_all(temp.path().join(name).join("bin")).expect("create installation");
    }
    let settings = Settings {
        installation_dir: temp.path().to_path_buf(),
        version: VersionReq::parse("=18.6.0").expect("parse requirement"),
        ..Settings::default()
    };
    let (logs, selected) = capture_debug_logs(|| resolve_installed_dir(&settings));
    assert_eq!(selected.is_some(), is_selected);
    let event = logs
        .iter()
        .find(|line| line.contains("resolved PostgreSQL installation directory"))
        .expect("selection event");
    assert!(event.contains(&format!("outcome=\"{outcome}\"")), "{event}");
    assert!(
        !event.contains(&temp.path().display().to_string()),
        "event must omit paths: {event}"
    );
}
