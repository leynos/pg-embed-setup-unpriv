//! A test cluster is non-durable by default, and `PG_EMBED_DURABLE=1` restores
//! `PostgreSQL`'s durable settings (#297).
//!
//! Each case re-runs this binary as a child that boots a real cluster through
//! the shared handle and reports what the running server says about its
//! durability settings, with the environment set only on that child's `Command`.
#![cfg(unix)]

use std::path::Path;

use diesel::{RunQueryDsl, sql_types::Text};
use pg_embedded_setup_unpriv::test_support;

#[path = "support/cluster_children.rs"]
#[expect(
    dead_code,
    reason = "this binary uses only the spawning half of the shared child harness"
)]
mod cluster_children;

use cluster_children::{child_mode, fixed_root, report, should_run, spawn_child};

/// The server settings a disposable cluster turns off.
const SETTINGS: [&str; 3] = ["fsync", "synchronous_commit", "full_page_writes"];

#[derive(diesel::QueryableByName)]
struct Setting {
    #[diesel(sql_type = Text)]
    value: String,
}

/// Returns `name=value` for each durability setting of the running server.
fn server_settings() -> Result<String, String> {
    let handle = test_support::shared_cluster_handle().map_err(|err| err.to_string())?;
    let mut connection = handle
        .connection()
        .diesel_connection("postgres")
        .map_err(|err| err.to_string())?;
    let mut parts = Vec::new();
    for name in SETTINGS {
        let rows: Vec<Setting> =
            diesel::sql_query(format!("SELECT current_setting('{name}') AS value"))
                .load(&mut connection)
                .map_err(|err| err.to_string())?;
        let value = rows.first().map_or("?", |row| row.value.as_str());
        parts.push(format!("{name}={value}"));
    }
    Ok(parts.join(" "))
}

/// Child: boot one cluster and report its durability settings.
#[test]
#[ignore = "run only as a child of the durability tests"]
fn cluster_child() {
    if child_mode().is_none() {
        return;
    }
    let line = match server_settings() {
        Ok(settings) => format!("settings {settings}"),
        Err(err) => format!("failed {}", err.replace('\n', " | ")),
    };
    report(&line).expect("stdout is writable");
}

/// Starts a child for `case`, with `extra` environment, and returns its report.
fn settings_reported(case: &str, extra: &[(&str, &Path)]) -> Result<String, String> {
    let root = fixed_root(case).map_err(|err| err.to_string())?;
    let mut child = spawn_child(&root, "settings", extra).map_err(|err| err.to_string())?;
    let said = child.report().map_err(|err| err.to_string())?;
    child.finish();
    Ok(said)
}

/// By default the running server has all three durability settings off.
#[test]
fn a_default_test_cluster_runs_with_durability_off() -> Result<(), String> {
    if !should_run() {
        return Ok(());
    }
    let said = settings_reported("durability_default", &[])?;
    if said == "settings fsync=off synchronous_commit=off full_page_writes=off" {
        return Ok(());
    }
    Err(format!(
        "expected a non-durable server, the child said: {said}"
    ))
}

/// With `PG_EMBED_DURABLE=1` the running server keeps `PostgreSQL`'s durable
/// defaults.
#[test]
fn the_opt_out_restores_the_durable_settings() -> Result<(), String> {
    if !should_run() {
        return Ok(());
    }
    let said = settings_reported(
        "durability_opt_out",
        &[("PG_EMBED_DURABLE", Path::new("1"))],
    )?;
    if said == "settings fsync=on synchronous_commit=on full_page_writes=on" {
        return Ok(());
    }
    Err(format!("expected a durable server, the child said: {said}"))
}
