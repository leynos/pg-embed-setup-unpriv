//! Regression coverage: a started cluster resolves named time zones.
//!
//! `PostgreSQL` archives from `18.6.1` onward pack most of `share/timezone` as
//! tar hard links, and the `postgresql_archive` release this crate builds
//! against extracts plain files, directories and symlinks but silently drops
//! hard links. Such a tree therefore has no `share/timezone/UTC`, and any
//! session that names a zone fails with
//! `invalid value for parameter "TimeZone": "UTC"` (leynos/mxd#665, and #351
//! here). The `18.6.0` archives predate that switch and were built against the
//! host zoneinfo database, so they carry no `share/timezone` at all and are
//! unaffected.
//!
//! [`PgEnvCfg`] pins its default version to `18.6.0` for that reason. This
//! suite proves the pin end to end: it starts one real cluster and asks the
//! server for three named zones. Moving the default back to a release whose
//! time zone files are hard links, or extracting one with an archive library
//! that still drops them, fails here rather than in a downstream project.
//!
//! The zones share one cluster on purpose: starting a cluster is the expensive
//! part, and every case exercises the same server. `rstest` cases would each
//! start their own, so the table is iterated inside the single cluster instead.
//!
//! [`PgEnvCfg`]: pg_embedded_setup_unpriv::PgEnvCfg
#![cfg(all(unix, feature = "diesel-support"))]
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use color_eyre::eyre::{Report, Result, ensure, eyre};
use diesel::{QueryableByName, RunQueryDsl, sql_query, sql_types::Text};
use pg_embedded_setup_unpriv::{BootstrapError, ClusterHandle, TestCluster};
use rstest::rstest;
use tracing::warn;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};

/// Zones every `PostgreSQL` build is expected to resolve.
///
/// `UTC` and `Etc/UTC` are hard links to `UCT` in the affected archives, so
/// they are absent from a tree whose hard links were dropped. `Europe/London`
/// is a regular file there and is included to show the failure is about the
/// link aliases rather than about zone data in general.
const ZONES: [&str; 3] = ["UTC", "Etc/UTC", "Europe/London"];

/// How much of the server log to attach to a failure.
const LOG_TAIL_BYTES: u64 = 2048;

/// A cluster that could not be started here, or a genuine test failure.
enum Failure {
    /// The environment cannot run an embedded cluster.
    Skipped(String),
    /// The behaviour under test is wrong.
    Failed(Report),
}

/// A row holding the single text column of a diagnostic query.
#[derive(QueryableByName)]
struct VersionRow {
    #[diesel(sql_type = Text)]
    version: String,
}

/// The current `TimeZone` setting as `SHOW TimeZone` reports it.
#[derive(QueryableByName)]
struct TimeZoneRow {
    #[diesel(sql_type = Text)]
    #[diesel(column_name = "TimeZone")]
    time_zone: String,
}

/// Every zone in [`ZONES`] is accepted by a freshly started cluster.
#[rstest]
fn a_started_cluster_resolves_nameable_time_zones(serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("timezone-regression")?;
    sandbox.reset()?;
    // Clearing TZ and TZDIR keeps the host's zone configuration out of the
    // answer: the server must resolve the names from its own data.
    let vars = sandbox.env_without_timezone();
    let outcome = sandbox.with_env(vars, verify_zones_against_live_cluster);
    drop(serial_guard);
    match outcome {
        Ok(()) => Ok(()),
        Err(Failure::Skipped(reason)) => {
            warn!("SKIP: {reason}");
            Ok(())
        }
        Err(Failure::Failed(report)) => Err(report),
    }
}

/// Starts one cluster, runs every zone case, and reports the first failure.
fn verify_zones_against_live_cluster() -> std::result::Result<(), Failure> {
    let (handle, guard) = TestCluster::new_split().map_err(|err| skip_or_fail(&err))?;
    let outcome = verify_every_zone(&handle);
    // Diagnostics need the server and its files, so they are gathered before
    // the guard stops the cluster.
    let failure = outcome.err().map(|report| {
        report.wrap_err(format!(
            "time zone regression probe failed:\n{}",
            collect_diagnostics(&handle)
        ))
    });
    drop(guard);
    failure.map_or(Ok(()), |report| Err(Failure::Failed(report)))
}

/// Runs `SET TimeZone` then `SHOW TimeZone` for every case in [`ZONES`].
fn verify_every_zone(handle: &ClusterHandle) -> Result<()> {
    let mut connection = handle
        .connection()
        .diesel_connection("postgres")
        .map_err(|err| eyre!(err))?;
    for zone in ZONES {
        // The zone names are compile-time constants, so interpolating one
        // cannot smuggle SQL into the statement.
        sql_query(format!("SET TimeZone = '{zone}'"))
            .execute(&mut connection)
            .map_err(|err| eyre!("the cluster rejected time zone {zone:?}: {err}"))?;
        let rows: Vec<TimeZoneRow> = sql_query("SHOW TimeZone")
            .load(&mut connection)
            .map_err(|err| eyre!("SHOW TimeZone failed for {zone:?}: {err}"))?;
        let Some(row) = rows.first() else {
            return Err(eyre!("SHOW TimeZone returned no rows for {zone:?}"));
        };
        ensure!(
            row.time_zone == zone,
            "the cluster resolved {zone:?} as {:?}",
            row.time_zone
        );
    }
    Ok(())
}

/// Gathers only the allowlisted facts that explain a time zone failure.
///
/// The server's settings, password, connection URL and command debug logs are
/// deliberately left out: this text ends up in test output.
fn collect_diagnostics(handle: &ClusterHandle) -> String {
    let install_dir = &handle.settings().installation_dir;
    let data_dir = &handle.settings().data_dir;
    let mut lines = vec![
        format!("server version: {}", server_version(handle)),
        format!("installation dir: {}", install_dir.display()),
    ];
    for entry in ["share/timezone", "share/timezone/UCT", "share/timezone/UTC"] {
        lines.push(format!(
            "{entry}: {}",
            describe(install_dir.join(entry).as_path())
        ));
    }
    lines.push(format!(
        "start.log tail ({}):\n{}",
        data_dir.join("start.log").display(),
        tail_of(&data_dir.join("start.log"))
    ));
    lines.join("\n")
}

/// Reports what `SELECT version()` returns, or why it could not be read.
fn server_version(handle: &ClusterHandle) -> String {
    let query = || -> Result<String> {
        let mut connection = handle
            .connection()
            .diesel_connection("postgres")
            .map_err(|err| eyre!(err))?;
        let rows: Vec<VersionRow> = sql_query("SELECT version()")
            .load(&mut connection)
            .map_err(|err| eyre!(err))?;
        rows.into_iter()
            .next()
            .map(|row| row.version)
            .ok_or_else(|| eyre!("SELECT version() returned no rows"))
    };
    query().unwrap_or_else(|err| format!("<unavailable: {err}>"))
}

/// Describes a path's type without following a final symlink.
fn describe(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = std::fs::read_link(path).map_or_else(
                |err| format!("<unreadable: {err}>"),
                |target| target.display().to_string(),
            );
            format!("symlink -> {target}")
        }
        Ok(metadata) if metadata.is_dir() => "directory".to_owned(),
        Ok(metadata) if metadata.is_file() => {
            format!("regular file ({} bytes)", metadata.len())
        }
        Ok(_) => "other file type".to_owned(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => "absent".to_owned(),
        Err(err) => format!("<stat failed: {err}>"),
    }
}

/// Returns at most [`LOG_TAIL_BYTES`] from the end of `path`.
///
/// The log is read as lossy UTF-8 so a partial multi-byte character at the
/// boundary cannot fail the diagnostic path itself.
fn tail_of(path: &Path) -> String {
    let read_tail = || -> std::io::Result<String> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let start = length.saturating_sub(LOG_TAIL_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(LOG_TAIL_BYTES).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    read_tail().unwrap_or_else(|err| format!("<unavailable: {err}>"))
}

/// Classifies a bootstrap error as an environment skip or a real failure.
fn skip_or_fail(err: &BootstrapError) -> Failure {
    let debug = format!("{err:?}");
    cluster_skip_message(&err.to_string(), Some(&debug))
        .map_or_else(|| Failure::Failed(eyre!("{err}")), Failure::Skipped)
}
