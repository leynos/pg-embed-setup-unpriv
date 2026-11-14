//! Test cluster fixture utilities for managing test worlds.

use std::{cell::RefCell, ffi::OsString, fs};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, eyre};
use nix::unistd::geteuid;
use pg_embedded_setup_unpriv::test_support::{ensure_worker_env, worker_binary_for_tests};
use rstest::fixture;

use super::{
    TestCluster,
    cap_fs,
    cluster_skip::cluster_skip_message,
    env::ScopedEnvVars,
    env_isolation::{override_env_os, override_env_path},
    process_utils::prepare_read_only_dir,
    sandbox::TestSandbox,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub(super) enum FixtureEnvProfile {
    #[default]
    Default,
    MissingTimezone,
    MissingWorkerBinary,
    NonExecutableWorkerBinary,
    PermissionDenied,
    ReadOnlyFilesystem,
    InvalidConfiguration,
}

pub(super) struct FixtureWorld {
    pub(super) sandbox: TestSandbox,
    cluster: Option<TestCluster>,
    panic_message: Option<String>,
    skip_reason: Option<String>,
    pub(super) env_profile: FixtureEnvProfile,
}

impl FixtureWorld {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("rstest-fixture").context("create fixture sandbox")?,
            cluster: None,
            panic_message: None,
            skip_reason: None,
            env_profile: FixtureEnvProfile::Default,
        })
    }

    pub(super) fn mark_skip(&mut self, reason: impl Into<String>) {
        let message = reason.into();
        tracing::warn!("{message}");
        self.skip_reason = Some(message);
    }

    pub(super) const fn is_skipped(&self) -> bool { self.skip_reason.is_some() }

    pub(super) fn ensure_not_skipped(&self) -> Result<()> {
        if self.is_skipped() {
            Err(eyre!("scenario skipped"))
        } else {
            Ok(())
        }
    }

    pub(super) fn record_cluster(&mut self, cluster: TestCluster) {
        self.cluster = Some(cluster);
        self.panic_message = None;
        self.skip_reason = None;
    }

    pub(super) fn record_failure(&mut self, message: String) {
        self.cluster = None;
        if let Some(reason) = cluster_skip_message(&message, None) {
            self.mark_skip(reason);
        } else {
            self.panic_message = Some(message);
        }
    }

    pub(super) fn cluster(&self) -> Result<&TestCluster> {
        self.ensure_not_skipped()?;
        self.cluster
            .as_ref()
            .ok_or_else(|| eyre!("test_cluster fixture did not yield a cluster"))
    }

    pub(super) fn panic_message(&self) -> Result<&str> {
        self.ensure_not_skipped()?;
        self.panic_message
            .as_deref()
            .ok_or_else(|| eyre!("fixture should have recorded a panic"))
    }
}

impl Drop for FixtureWorld {
    fn drop(&mut self) { drop(self.cluster.take()); }
}

pub(super) type FixtureWorldFixture = Result<RefCell<FixtureWorld>>;

pub(super) fn borrow_world(world: &FixtureWorldFixture) -> Result<&RefCell<FixtureWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("fixture world failed to initialize: {err}")))
}

#[fixture]
pub(super) fn world() -> FixtureWorldFixture {
    let world = FixtureWorld::new()?;
    Ok(RefCell::new(world))
}

pub(super) fn env_for_profile(
    sandbox: &TestSandbox,
    profile: FixtureEnvProfile,
) -> Result<ScopedEnvVars> {
    match profile {
        FixtureEnvProfile::Default => Ok(sandbox.env_without_timezone()),
        FixtureEnvProfile::MissingTimezone => {
            let missing_dir = sandbox.install_dir().join("missing-tz");
            Ok(sandbox.env_with_timezone_override(missing_dir.as_ref()))
        }
        FixtureEnvProfile::MissingWorkerBinary => Ok(env_with_missing_worker(sandbox)),
        FixtureEnvProfile::NonExecutableWorkerBinary => env_with_non_executable_worker(sandbox),
        FixtureEnvProfile::PermissionDenied => env_with_denied_dirs(sandbox),
        FixtureEnvProfile::ReadOnlyFilesystem => env_with_read_only_dirs(sandbox),
        FixtureEnvProfile::InvalidConfiguration => {
            let mut vars = sandbox.env_without_timezone();
            override_env_os(&mut vars, "PG_PORT", Some(OsString::from("not-a-port")));
            Ok(vars)
        }
    }
}
/// Points `PG_EMBEDDED_WORKER` at a worker path that does not exist.
fn env_with_missing_worker(sandbox: &TestSandbox) -> ScopedEnvVars {
    let mut vars = sandbox.env_without_timezone();
    let fake_worker = sandbox
        .install_dir()
        .join("missing-worker")
        .join("pg_worker");
    override_env_os(
        &mut vars,
        "PG_EMBEDDED_WORKER",
        Some(OsString::from(fake_worker.as_str())),
    );
    vars
}

/// Stages a stub worker without the execution bit and points the environment
/// at it.
fn env_with_non_executable_worker(sandbox: &TestSandbox) -> Result<ScopedEnvVars> {
    let mut vars = sandbox.env_without_timezone();
    let worker_dir = sandbox.install_dir().join("non-exec-worker");
    let worker_path = worker_dir.join("pg_worker");
    std::fs::create_dir_all(worker_dir.as_std_path())
        .with_context(|| format!("create non-exec worker dir at {worker_dir}"))?;
    fs::write(worker_path.as_std_path(), "#!/bin/sh\nexit 0\n")
        .with_context(|| format!("write stub worker at {worker_path}"))?;
    cap_fs::set_permissions(worker_path.as_ref(), 0o644)
        .with_context(|| format!("strip execution bit from {worker_path}"))?;
    override_env_os(
        &mut vars,
        "PG_EMBEDDED_WORKER",
        Some(OsString::from(worker_path.as_str())),
    );
    Ok(vars)
}

/// Routes the runtime and data directories at mode-0o000 directories.
fn env_with_denied_dirs(sandbox: &TestSandbox) -> Result<ScopedEnvVars> {
    env_with_prepared_dirs(sandbox, "denied", |path| create_dir_with_mode(path, 0o000))
}

/// Routes the runtime and data directories at read-only directories.
fn env_with_read_only_dirs(sandbox: &TestSandbox) -> Result<ScopedEnvVars> {
    env_with_prepared_dirs(sandbox, "readonly", prepare_read_only_dir)
}

/// Routes the runtime and data directories at directories prepared by
/// `prepare`, named `<prefix>-runtime` and `<prefix>-data`.
fn env_with_prepared_dirs(
    sandbox: &TestSandbox,
    prefix: &str,
    prepare: impl Fn(&Utf8PathBuf) -> Result<()>,
) -> Result<ScopedEnvVars> {
    let mut vars = sandbox.env_without_timezone();
    let runtime = sandbox.install_dir().join(format!("{prefix}-runtime"));
    let data = sandbox.install_dir().join(format!("{prefix}-data"));
    prepare(&runtime)?;
    prepare(&data)?;
    override_env_path(&mut vars, "PG_RUNTIME_DIR", runtime.as_ref());
    override_env_path(&mut vars, "PG_DATA_DIR", data.as_ref());
    Ok(vars)
}

fn create_dir_with_mode(path: &Utf8PathBuf, mode: u32) -> Result<()> {
    std::fs::create_dir_all(path.as_std_path())
        .with_context(|| format!("create permission sandbox dir at {path}"))?;
    cap_fs::set_permissions(path.as_ref(), mode)
        .with_context(|| format!("set permissions {mode:o} for {path}"))
}

/// Return whether a run must locate a worker binary itself: only a root run
/// with no configured worker does.
const fn needs_discovered_worker(is_root: bool, worker_configured: bool) -> bool {
    is_root && !worker_configured
}

/// Return whether `ensure_worker_env` would panic for want of a worker.
///
/// `find_worker` is only called when discovery is needed, as in the fixture,
/// because looking for a worker stages the binary as a side effect.
pub(super) fn worker_is_missing(
    is_root: bool,
    worker_configured: bool,
    find_worker: impl FnOnce() -> bool,
) -> bool {
    needs_discovered_worker(is_root, worker_configured) && !find_worker()
}

/// Start the fixture's cluster, returning the failure message instead of
/// panicking.
///
/// This mirrors `test_support::test_cluster`, whose failure path panics with
/// `SKIP-TEST-CLUSTER`. Calling the fallible constructor directly lets the
/// tests inspect the failure as data, with no `catch_unwind`. The fixture's
/// own wrapper is no longer exercised by these tests: the library's unit tests
/// cover the missing-worker panic (`resolve_worker_path`) and
/// `SKIP-TEST-CLUSTER` handling, but not the constructor-error mapping, which
/// this helper repeats.
pub(super) fn try_test_cluster() -> std::result::Result<TestCluster, String> {
    // `ensure_worker_env` panics with `SKIP-TEST-CLUSTER` when a root run has
    // no worker binary to find. Check that condition first and return it as
    // data, so the helper never panics where the fixture would have skipped.
    if worker_is_missing(
        geteuid().is_root(),
        std::env::var_os("PG_EMBEDDED_WORKER").is_some(),
        || worker_binary_for_tests().is_some(),
    ) {
        return Err(
            "SKIP-TEST-CLUSTER: PG_EMBEDDED_WORKER is not set and pg_worker binary was not found"
                .to_owned(),
        );
    }
    let worker_guard = ensure_worker_env();
    TestCluster::new()
        .map(|cluster| cluster.with_worker_guard(worker_guard))
        .map_err(|err| {
            format!("SKIP-TEST-CLUSTER: test_cluster fixture failed to start PostgreSQL: {err:?}")
        })
}

pub(super) fn handle_fixture_failure(message: String) -> Result<()> {
    cluster_skip_message(&message, None).map_or_else(
        || Err(eyre!(message)),
        |reason| {
            tracing::warn!("{reason}");
            Ok(())
        },
    )
}
