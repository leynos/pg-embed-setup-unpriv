//! Test utilities and unit checks for cluster fixtures.

use color_eyre::eyre::{Context, Result, ensure};
use pg_embedded_setup_unpriv::TestCluster;
use rstest::rstest;

use super::{
    TestSandbox,
    env_isolation::{EnvIsolationGuard, set_env_var},
    process_utils::{
        read_postmaster_pid,
        sandbox_root_path,
        wait_for_pid_file_removal,
        wait_for_process_exit,
    },
    serial::{ScenarioSerialGuard, serial_guard},
};

pub(super) fn run_unit_fixture_test<F>(name: &str, test: F) -> Result<()>
where
    F: FnOnce(TestCluster) -> Result<()>,
{
    let _env_reset = EnvIsolationGuard::capture();
    let sandbox =
        TestSandbox::new(name).context("create rstest fixture sandbox for unit coverage")?;
    sandbox
        .reset()
        .context("reset rstest fixture sandbox before applying env")?;
    let result = sandbox.with_env(
        sandbox.env_without_timezone(),
        super::world::try_test_cluster,
    );
    result.map_or_else(super::world::handle_fixture_failure, test)
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
pub(super) fn fixture_exposes_connection_metadata(
    _serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    run_unit_fixture_test("rstest-fixture-metadata", |test_cluster| {
        let metadata = test_cluster.connection().metadata();
        ensure!(
            metadata.port() > 0,
            "fixture should yield a running PostgreSQL instance",
        );
        ensure!(
            metadata.superuser() == "postgres",
            "tests rely on deterministic sandbox credentials",
        );
        Ok(())
    })
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
pub(super) fn fixture_reuses_cluster_environment(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    run_unit_fixture_test("rstest-fixture-env", |test_cluster| {
        let env = test_cluster.environment();
        ensure!(
            env.pgpass_file.starts_with(env.home.as_str()),
            "pgpass file should live under HOME for the fixture",
        );
        ensure!(
            test_cluster.settings().data_dir.exists(),
            "data directory should exist whilst the cluster runs",
        );
        Ok(())
    })
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
pub(super) fn fixture_environment_variable_isolation(
    _serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    const LEAK_VAR: &str = "RSTEST_CLUSTER_ENV_POLLUTION";
    run_unit_fixture_test("rstest-fixture-env-pollution", |_test_cluster| {
        unsafe { set_env_var(LEAK_VAR, "polluted_value") };
        Ok(())
    })?;
    ensure!(
        std::env::var(LEAK_VAR).is_err(),
        "{leak} should not leak across fixture runs",
        leak = LEAK_VAR,
    );
    run_unit_fixture_test("rstest-fixture-env-clean", |_test_cluster| {
        ensure!(
            std::env::var(LEAK_VAR).is_err(),
            "{leak} should be absent when the fixture starts",
            leak = LEAK_VAR,
        );
        Ok(())
    })
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
pub(super) fn fixture_teardown_resource_cleanup(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("rstest-fixture-teardown")
        .context("create sandbox for teardown coverage")?;
    sandbox
        .reset()
        .context("reset sandbox before teardown coverage")?;
    let sandbox_root = sandbox_root_path(&sandbox)?;
    let result = sandbox.with_env(
        sandbox.env_without_timezone(),
        super::world::try_test_cluster,
    );
    let cluster = match result {
        Ok(cluster) => cluster,
        Err(message) => {
            super::world::handle_fixture_failure(message)?;
            return Ok(());
        }
    };
    let data_dir = cluster.settings().data_dir.clone();
    let pid = read_postmaster_pid(data_dir.as_path())?;
    drop(cluster);
    wait_for_process_exit(pid)?;
    wait_for_pid_file_removal(data_dir.as_path())?;
    drop(sandbox);
    ensure!(
        !sandbox_root.exists(),
        "sandbox root should be removed once the guard drops",
    );
    Ok(())
}

/// The returned failure messages match the fixture's panic messages, which is
/// what lets the scenarios assert on them as they did on the panic payload.
#[rstest]
fn failure_messages_match_the_fixtures_panic_messages() {
    assert_eq!(
        super::world::startup_failure_message(&"boom"),
        "SKIP-TEST-CLUSTER: test_cluster fixture failed to start PostgreSQL: \"boom\"",
    );
    assert_eq!(
        super::world::MISSING_WORKER_MESSAGE,
        "SKIP-TEST-CLUSTER: PG_EMBEDDED_WORKER is not set and pg_worker binary was not found",
    );
}

/// The missing-worker condition is the one `ensure_worker_env` panics on, and
/// the worker lookup, which stages the binary, only runs when it is needed.
#[rstest]
#[case::root_without_worker(true, false, false, true)]
#[case::root_with_discoverable_worker(true, false, true, false)]
#[case::root_with_configured_worker(true, true, false, false)]
#[case::unprivileged(false, false, false, false)]
fn worker_is_missing_only_for_a_root_run_that_finds_none(
    #[case] is_root: bool,
    #[case] worker_configured: bool,
    #[case] worker_found: bool,
    #[case] expected: bool,
) {
    let lookup_runs = is_root && !worker_configured;
    let lookup_ran = std::cell::Cell::new(false);

    let missing = super::world::worker_is_missing(is_root, worker_configured, || {
        lookup_ran.set(true);
        worker_found
    });

    assert_eq!(missing, expected, "missing-worker decision");
    assert_eq!(lookup_ran.get(), lookup_runs, "lookup only when needed");
}
