//! Real-cluster proof that the extension hook places a loadable module.
//!
//! The fixture extension is packaged from the downloaded Theseus tree itself:
//! `lib/autoinc.so` is copied to `lib/df12_probe.so` and paired with a control
//! file and an SQL script that binds `df12_probe_autoinc()` to the `autoinc`
//! symbol. Nothing is compiled. `CREATE FUNCTION ... LANGUAGE C` loads the
//! shared object from `$libdir`, so a successful `CREATE EXTENSION` proves the
//! hook installed both the module and the control files where the server
//! looks.
#![cfg(unix)]

use std::ffi::OsString;

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result, ensure, eyre};
use flate2::{Compression, write::GzEncoder};
use pg_embedded_setup_unpriv::{
    TestCluster,
    extensions::{Sha256Hex, compile_target},
};
use postgres::NoTls;

// This test uses a subset of the shared sandbox helpers; the rest are
// exercised by the other behavioural suites that include the same files.
#[expect(
    dead_code,
    reason = "shared support module; only remove_tree is unused here"
)]
#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/env.rs"]
mod env;
#[expect(
    dead_code,
    reason = "shared support module; this test needs only new, install_dir, base_env and with_env"
)]
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use sandbox::TestSandbox;
use serial::serial_guard;

const PROBE: &str = "df12_probe";
/// Shared-object suffix `PostgreSQL` uses on this platform.
const MODULE_SUFFIX: &str = if cfg!(target_os = "macos") {
    "dylib"
} else {
    "so"
};

/// Starts a cluster in the sandbox, treating known bootstrap limitations as a skip.
///
/// Returns `Ok(Some(cluster))` when `PostgreSQL` started, `Ok(None)` when the
/// bootstrap reported a `SKIP-TEST-CLUSTER` condition (no binaries, no
/// network), and an error for any other bootstrap failure.
///
/// ```ignore
/// let sandbox = TestSandbox::new("probe")?;
/// let Some(cluster) = start_cluster(&sandbox, Vec::new())? else { return Ok(()) };
/// ```
fn start_cluster(
    sandbox: &TestSandbox,
    extra: Vec<(OsString, Option<OsString>)>,
) -> Result<Option<TestCluster>> {
    let mut vars = sandbox.base_env();
    vars.extend(extra);
    match sandbox.with_env(vars, TestCluster::new) {
        Ok(cluster) => Ok(Some(cluster)),
        Err(err) => {
            let message = err.to_string();
            if let Some(reason) = cluster_skip_message(&message, Some(&format!("{err:?}"))) {
                tracing::warn!("{reason}");
                return Ok(None);
            }
            Err(eyre!("cluster bootstrap failed: {err}"))
        }
    }
}

/// Packages the tree's `autoinc` module as the probe extension.
///
/// Copies `lib/autoinc.<suffix>` from `install_dir` into a gzip tar as
/// `lib/df12_probe.<suffix>` with a control file and an SQL script, writes the
/// archive under `<out_dir>/ext-cache/<sha256>/df12_probe.tar.gz` (the layout
/// the hook's cache expects), and returns the cache directory and the archive
/// bytes. Fails when the module is missing or the files cannot be written.
///
/// ```ignore
/// let (cache_dir, bytes) = package_probe(&install_dir, &out_dir)?;
/// assert!(cache_dir.ends_with("ext-cache"));
/// ```
fn package_probe(install_dir: &Utf8Path, out_dir: &Utf8Path) -> Result<(Utf8PathBuf, Vec<u8>)> {
    let module_name = format!("autoinc.{MODULE_SUFFIX}");
    let module = std::fs::read(install_dir.join("lib").join(&module_name))
        .with_context(|| format!("read {module_name} from the Theseus tree"))?;
    let control = format!(
        "comment = 'df12 probe'\ndefault_version = '1.0'\nmodule_pathname = \
         '$libdir/{PROBE}'\nrelocatable = true\n"
    );
    let sql = format!(
        "CREATE FUNCTION {PROBE}_autoinc() RETURNS trigger AS 'MODULE_PATHNAME', 'autoinc' \
         LANGUAGE C;\n"
    );
    let probe_module = format!("lib/{PROBE}.{MODULE_SUFFIX}");
    let entries: [(&str, &[u8]); 3] = [
        (probe_module.as_str(), &module),
        ("share/extension/df12_probe.control", control.as_bytes()),
        ("share/extension/df12_probe--1.0.sql", sql.as_bytes()),
    ];
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (name, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        builder.append_data(&mut header, name, body)?;
    }
    let bytes = builder.into_inner()?.finish()?;
    let digest = Sha256Hex::of_bytes(&bytes);
    let cache_dir = out_dir.join("ext-cache");
    let entry_dir = cache_dir.join(digest.as_str());
    std::fs::create_dir_all(&entry_dir)?;
    std::fs::write(entry_dir.join("df12_probe.tar.gz"), &bytes)?;
    Ok((cache_dir, bytes))
}

/// Writes a schema-1 manifest describing the probe archive for `version` on
/// this crate's compile target and returns its path.
///
/// The URL is unreachable on purpose: the archive must come from the cache
/// `package_probe` seeded, so a cache miss fails the test rather than
/// downloading anything. Fails only when the file cannot be written.
///
/// ```ignore
/// let manifest = write_manifest(&out_dir, "17.11.0", &bytes)?;
/// assert!(manifest.ends_with("manifest.json"));
/// ```
fn write_manifest(out_dir: &Utf8Path, version: &str, bytes: &[u8]) -> Result<Utf8PathBuf> {
    let manifest = serde_json::json!({
        "schema_version": 1,
        "release": "v0.0.0-test",
        "generated_at": "2026-09-05T00:00:00+00:00",
        "extensions": [{
            "name": PROBE,
            "package": "df12-probe",
            "version": "1.0.0",
            "source": {
                "repository": "https://example.invalid/df12-probe",
                "tag": "v1.0.0",
                "commit": "0123456789abcdef0123456789abcdef01234567",
            },
            "artifacts": [{
                "postgresql": version,
                "target": compile_target(),
                "file": "df12_probe.tar.gz",
                "url": "http://127.0.0.1:9/unreachable/df12_probe.tar.gz",
                "sha256": Sha256Hex::of_bytes(bytes).as_str(),
                "size": bytes.len(),
                "files": [format!("lib/{PROBE}.{MODULE_SUFFIX}"), "share/extension/df12_probe--1.0.sql".to_owned(), "share/extension/df12_probe.control".to_owned()],
            }],
        }],
    });
    let path = out_dir.join("manifest.json");
    std::fs::write(&path, manifest.to_string())?;
    Ok(path)
}

/// Returns the versioned installation directory of a running cluster and its
/// name (the Theseus release, for example `17.11.0`).
///
/// Fails when the directory is not UTF-8 or has no `bin/`, which would mean
/// the settings do not point at an installed tree.
///
/// ```ignore
/// let (install_dir, version) = versioned_install_dir(&cluster)?;
/// assert!(install_dir.join("bin").is_dir());
/// ```
fn versioned_install_dir(cluster: &TestCluster) -> Result<(Utf8PathBuf, String)> {
    let dir = Utf8PathBuf::from_path_buf(cluster.settings().installation_dir.clone())
        .map_err(|path| eyre!("installation dir is not UTF-8: {}", path.display()))?;
    let version = dir
        .file_name()
        .ok_or_else(|| eyre!("installation dir has no name"))?
        .to_owned();
    ensure!(
        dir.join("bin").is_dir(),
        "installation dir {dir} has no bin/"
    );
    Ok((dir, version))
}

/// Everything a second cluster needs to install the probe through the hook.
struct ProbeAssets {
    extra_env: Vec<(OsString, Option<OsString>)>,
}

/// Starts a plain cluster, packages the probe from its tree, and returns the
/// environment that declares it. `None` means the cluster was skipped.
fn prepare_probe(sandbox: &TestSandbox) -> Result<Option<ProbeAssets>> {
    let Some(first) = start_cluster(sandbox, Vec::new())? else {
        return Ok(None);
    };
    let (install_dir, version) = versioned_install_dir(&first)?;
    let out_dir = sandbox.install_dir().join("probe-assets");
    std::fs::create_dir_all(&out_dir)?;
    let (cache_dir, bytes) = package_probe(&install_dir, &out_dir)?;
    let manifest = write_manifest(&out_dir, &version, &bytes)?;
    drop(first);
    Ok(Some(ProbeAssets {
        extra_env: vec![
            (OsString::from("PG_EXTENSIONS"), Some(OsString::from(PROBE))),
            (
                OsString::from("PG_EXTENSIONS_MANIFEST"),
                Some(OsString::from(manifest.as_str())),
            ),
            (
                OsString::from("PG_EXTENSIONS_CACHE_DIR"),
                Some(OsString::from(cache_dir.as_str())),
            ),
            (OsString::from("PG_EXTENSIONS_MANIFEST_SHA256"), None),
        ],
    }))
}

/// Asserts the hook reported the probe and the server can load it.
fn assert_probe_loaded(cluster: &pg_embedded_setup_unpriv::ClusterHandle) -> Result<()> {
    let installed = cluster.installed_extensions();
    ensure!(
        installed.len() == 1,
        "expected one installed extension, got {installed:?}"
    );
    let report = installed.first().ok_or_else(|| eyre!("empty report"))?;
    ensure!(
        report.name.as_str() == PROBE,
        "unexpected name {}",
        report.name
    );
    ensure!(report.files.len() == 3, "expected three files");

    let url = cluster.connection().database_url("postgres");
    let mut client = postgres::Client::connect(&url, NoTls).context("connect to the cluster")?;
    client.batch_execute(&format!("CREATE EXTENSION {PROBE}"))?;
    let row = client.query_one(
        "SELECT count(*) FROM pg_proc WHERE proname = $1",
        &[&format!("{PROBE}_autoinc")],
    )?;
    let count: i64 = row.get(0);
    ensure!(
        count == 1,
        "the probe function must exist after CREATE EXTENSION"
    );
    Ok(())
}

/// The hook installs a fixture module that the server loads with `CREATE EXTENSION`.
#[rstest::rstest]
fn hook_installs_a_loadable_extension(serial_guard: serial::ScenarioSerialGuard) -> Result<()> {
    let _serial = serial_guard;
    let sandbox = TestSandbox::new("extensions-probe")?;
    let Some(assets) = prepare_probe(&sandbox)? else {
        return Ok(());
    };
    let Some(cluster) = start_cluster(&sandbox, assets.extra_env)? else {
        return Ok(());
    };
    assert_probe_loaded(&cluster)
}

/// The async lifecycle runs the same hook and the server loads the module.
#[cfg(feature = "async-api")]
#[rstest::rstest]
fn hook_installs_a_loadable_extension_async(
    serial_guard: serial::ScenarioSerialGuard,
) -> Result<()> {
    let _serial = serial_guard;
    let sandbox = TestSandbox::new("extensions-probe-async")?;
    let Some(assets) = prepare_probe(&sandbox)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut vars = sandbox.base_env();
    vars.extend(assets.extra_env);
    let outcome = sandbox.with_env(vars, || {
        runtime.block_on(async { TestCluster::start_async().await })
    });
    let cluster = match outcome {
        Ok(cluster) => cluster,
        Err(err) => {
            let message = err.to_string();
            if let Some(reason) = cluster_skip_message(&message, Some(&format!("{err:?}"))) {
                tracing::warn!("{reason}");
                return Ok(());
            }
            return Err(eyre!("async cluster bootstrap failed: {err}"));
        }
    };
    assert_probe_loaded(&cluster)?;
    runtime
        .block_on(cluster.stop_async())
        .map_err(|err| eyre!("stop: {err}"))
}
