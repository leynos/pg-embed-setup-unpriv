//! Tests for the effective PostgreSQL version diagnostic.

use pg_embedded_setup_unpriv::{PgEnvCfg, test_support::capture_debug_logs};
use rstest::rstest;

#[rstest]
#[case::default(None, "default", "*")]
#[case::configured(Some("=17.4.0"), "configured", "=17.4.0")]
fn version_resolution_reports_source_and_requirement(
    #[case] requirement: Option<&str>,
    #[case] source: &str,
    #[case] expected: &str,
) {
    let cfg = PgEnvCfg {
        version_req: requirement.map(str::to_owned),
        ..PgEnvCfg::default()
    };
    let (logs, settings) = capture_debug_logs(|| cfg.to_settings());
    assert!(settings.is_ok(), "configuration should resolve");
    let event = logs
        .iter()
        .find(|line| line.contains("resolved PostgreSQL version requirement"))
        .expect("version resolution event");
    assert!(event.contains(&format!("source=\"{source}\"")), "{event}");
    assert!(event.contains(expected), "{event}");
}

#[test]
fn version_resolution_truncates_long_requirements_in_diagnostics() {
    let requirement = format!("=18.6.0-{}", "a".repeat(200));
    let cfg = PgEnvCfg {
        version_req: Some(requirement.clone()),
        ..PgEnvCfg::default()
    };
    let (logs, settings) = capture_debug_logs(|| cfg.to_settings());
    assert!(settings.is_ok(), "long prerelease requirement should parse");
    let event = logs
        .iter()
        .find(|line| line.contains("resolved PostgreSQL version requirement"))
        .expect("version resolution event");
    assert!(event.contains("requirement_truncated=true"), "{event}");
    assert!(
        !event.contains(&requirement),
        "diagnostic must truncate long requirements"
    );
}
