//! Compile-time fixture for the password-reuse public surface.
//!
//! `tests/ui.rs` uses this as a non-Windows trybuild pass fixture and as a
//! directly included Windows smoke-compile module, so the crate-root exports
//! `stored_cluster_password`, `reuse_existing_password` and
//! `PasswordReuseOutcome`, and the three password-reuse `BootstrapErrorKind`
//! variants, stay reachable and exhaustively matchable from a consumer crate.

use camino::Utf8Path;
use pg_embedded_setup_unpriv::{
    BootstrapErrorKind,
    BootstrapResult,
    PasswordReuseOutcome,
    PgEnvCfg,
    reuse_existing_password,
    stored_cluster_password,
};

/// Names every outcome without a wildcard.
///
/// Adding a variant therefore breaks this fixture rather than silently
/// changing what a consumer's exhaustive match does.
fn describe_outcome(outcome: PasswordReuseOutcome) -> &'static str {
    match outcome {
        PasswordReuseOutcome::Reused => "reused",
        PasswordReuseOutcome::ExplicitPassword => "explicit",
        PasswordReuseOutcome::NoCluster => "no cluster",
    }
}

/// Names the three password-reuse error kinds a consumer branches on.
///
/// The wildcard is deliberate: the enum carries unrelated variants, and this
/// fixture is about the ones this feature added.
fn describe_kind(kind: BootstrapErrorKind) -> &'static str {
    match kind {
        BootstrapErrorKind::ClusterPasswordMissing => "missing",
        BootstrapErrorKind::ClusterPasswordUnreadable => "unreadable",
        BootstrapErrorKind::ClusterPasswordEmpty => "empty",
        _ => "other",
    }
}

/// Exercises the public signatures against a directory that holds no cluster.
///
/// # Errors
///
/// Returns an error when the settings cannot be built, or when the query or
/// the command fails against a path that should simply report no cluster.
pub fn verify_surface() -> BootstrapResult<()> {
    let base = Utf8Path::new("target/nonexistent-password-reuse-ui-fixture");
    let data_dir = base.join("data");
    let password_file = base.join(".pgpass");

    // No PG_VERSION marker, so the query reports no cluster and never opens
    // the password file.
    assert!(stored_cluster_password(&data_dir, &password_file)?.is_none());

    let cfg = PgEnvCfg::default();
    let mut settings = cfg.to_settings()?;
    let outcome = reuse_existing_password(&mut settings, &data_dir, &password_file, false)?;
    assert_eq!(outcome, PasswordReuseOutcome::NoCluster);
    assert_eq!(describe_outcome(outcome), "no cluster");

    // An explicit password takes the other branch without touching the file.
    let explicit = reuse_existing_password(&mut settings, &data_dir, &password_file, true)?;
    assert_eq!(explicit, PasswordReuseOutcome::ExplicitPassword);
    assert_eq!(describe_outcome(PasswordReuseOutcome::Reused), "reused");

    assert_eq!(
        describe_kind(BootstrapErrorKind::ClusterPasswordMissing),
        "missing"
    );
    assert_eq!(
        describe_kind(BootstrapErrorKind::ClusterPasswordUnreadable),
        "unreadable"
    );
    assert_eq!(
        describe_kind(BootstrapErrorKind::ClusterPasswordEmpty),
        "empty"
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() -> BootstrapResult<()> { verify_surface() }
