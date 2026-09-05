//! Extension hook ordering: install happens after `Setup` and before `Start`
//! in the root, async-root and setup-only lifecycles.

use std::sync::{Arc, Mutex};

use camino::Utf8Path;
use color_eyre::eyre::{Result, ensure, eyre};
use rstest::rstest;
use serial_test::serial;

use super::*;
use crate::cluster::extension_hook::populate_cache_on_miss;

/// Names of the fixture files the ordering tests install.
const PROBE_FILES: [(&str, &[u8]); 2] = [
    ("lib/probe.so", b"module"),
    ("share/extension/probe.control", b"default_version = '1'\n"),
];

/// A worker-operation hook that creates the versioned tree on `Setup` and
/// records, at `Start`, whether the extension files were already present.
struct OrderingHook {
    operations: Arc<Mutex<Vec<String>>>,
    _hook_guard: crate::test_support::HookGuard,
}

fn ordering_hook(install_root: &Utf8Path) -> Result<OrderingHook> {
    let operations = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&operations);
    let versioned = install_root.join(TEST_POSTGRES_VERSION);
    let hook_guard = install_run_root_operation_hook(move |_, _, operation| {
        let label = match operation {
            crate::cluster::WorkerOperation::Setup => {
                for sub in ["bin", "lib", "share/extension"] {
                    fs::create_dir_all(versioned.join(sub).as_std_path())
                        .map_err(|err| eyre!("create {sub}: {err}"))?;
                }
                "setup".to_owned()
            }
            crate::cluster::WorkerOperation::Start => {
                let present = PROBE_FILES
                    .iter()
                    .all(|(name, _)| versioned.join(name).is_file());
                format!("start(extension_present={present})")
            }
            other => other.as_str().to_owned(),
        };
        recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(label);
        Ok(())
    })?;
    Ok(OrderingHook {
        operations,
        _hook_guard: hook_guard,
    })
}

/// Writes a probe archive into an extension cache plus a manifest, and returns
/// the request that declares it.
fn probe_request(base: &Utf8Path) -> Result<crate::extensions::ExtensionRequest> {
    use crate::extensions::{ExtensionName, ExtensionRequest, ManifestSource, Sha256Hex};
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (name, body) in PROBE_FILES {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        builder.append_data(&mut header, name, body)?;
    }
    let bytes = builder.into_inner()?.finish()?;
    let digest = Sha256Hex::of_bytes(&bytes);
    let cache_dir = base.join("ext-cache");
    let entry = cache_dir.join(digest.as_str());
    fs::create_dir_all(entry.as_std_path())?;
    fs::write(entry.join("probe.tar.gz").as_std_path(), &bytes)?;
    let files: Vec<&str> = PROBE_FILES.iter().map(|(name, _)| *name).collect();
    let manifest = serde_json::json!({
        "schema_version": 1,
        "release": "v0.0.0-test",
        "generated_at": "2026-09-05T00:00:00+00:00",
        "extensions": [{
            "name": "probe",
            "package": "probe",
            "version": "1.0.0",
            "source": {"repository": "https://example.invalid/probe", "tag": "v1", "commit": "0123456789abcdef0123456789abcdef01234567"},
            "artifacts": [{
                "postgresql": TEST_POSTGRES_VERSION,
                "target": crate::extensions::compile_target(),
                "file": "probe.tar.gz",
                "url": "https://example.invalid/probe.tar.gz",
                "sha256": digest.as_str(),
                "size": bytes.len(),
                "files": files,
            }],
        }],
    });
    let manifest_path = base.join("manifest.json");
    fs::write(manifest_path.as_std_path(), manifest.to_string())?;
    Ok(ExtensionRequest {
        names: vec![ExtensionName::new("probe").map_err(|err| eyre!("{err}"))?],
        manifest: ManifestSource::Path {
            path: manifest_path,
            sha256: None,
        },
        cache_dir,
    })
}

fn ordering_bootstrap(paths: &RootSetupPaths) -> Result<TestBootstrapSettings> {
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &paths.install_dir,
        &paths.data_dir,
        &paths.scoped_cache_home,
    )?;
    bootstrap.extensions = Some(probe_request(&paths.install_dir)?);
    Ok(bootstrap)
}

fn assert_installed_between_setup_and_start(
    operations: &Mutex<Vec<String>>,
    bootstrap: &TestBootstrapSettings,
) -> Result<()> {
    let recorded = operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    ensure!(
        recorded == ["setup", "start(extension_present=true)"],
        "expected install between Setup and Start, recorded {recorded:?}"
    );
    ensure!(
        bootstrap.installed_extensions.len() == 1,
        "the handle must report the installed extension"
    );
    Ok(())
}

/// The synchronous root lifecycle installs extensions after Setup and before Start.
#[rstest]
#[serial(worker_hook)]
fn root_lifecycle_installs_extensions_between_setup_and_start(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let hook = ordering_hook(&paths.install_dir)?;
    let bootstrap = ordering_bootstrap(&paths)?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;
    let outcome = start_postgres(&runtime, bootstrap, &env_vars, &cache_config)?;
    assert_installed_between_setup_and_start(&hook.operations, &outcome.bootstrap)
}

/// The asynchronous root lifecycle keeps the same ordering.
#[cfg(feature = "async-api")]
#[rstest]
#[serial(worker_hook)]
fn async_root_lifecycle_installs_extensions_between_setup_and_start(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let hook = ordering_hook(&paths.install_dir)?;
    let bootstrap = ordering_bootstrap(&paths)?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;
    let outcome = runtime.block_on(start_postgres_async(bootstrap, &env_vars, &cache_config))?;
    assert_installed_between_setup_and_start(&hook.operations, &outcome.bootstrap)
}

/// The CLI setup-only lifecycle installs extensions after Setup without starting.
#[rstest]
#[serial(worker_hook)]
fn setup_only_lifecycle_installs_extensions_after_setup(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let hook = ordering_hook(&paths.install_dir)?;
    let bootstrap = ordering_bootstrap(&paths)?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;
    let prepared = setup_lifecycle(&runtime, bootstrap, &env_vars, &cache_config)?;
    let recorded = hook
        .operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    ensure!(
        recorded == ["setup"],
        "setup-only must not start: {recorded:?}"
    );
    let versioned = paths.install_dir.join(TEST_POSTGRES_VERSION);
    ensure!(
        PROBE_FILES
            .iter()
            .all(|(name, _)| versioned.join(name).is_file()),
        "extension files must be installed by the setup-only path"
    );
    ensure!(
        prepared.installed_extensions.len() == 1,
        "report carried through"
    );
    Ok(())
}

/// Binary-cache population runs only on a miss.
#[rstest]
fn populate_cache_on_miss_only_populates_on_cache_miss(
    #[from(cache_population_paths)] cache_population_paths_res: Result<CachePopulationPaths>,
) -> Result<()> {
    let cache_population_paths = cache_population_paths_res?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    bootstrap.settings.installation_dir = cache_population_paths
        .install_dir
        .clone()
        .into_std_path_buf();
    bootstrap.settings.data_dir = cache_population_paths.data_dir.into_std_path_buf();
    let cache_config = BinaryCacheConfig::with_dir(cache_population_paths.cache_dir.clone());

    let hit = PostSetup {
        cache_config: &cache_config,
        cache_hit: true,
    };
    populate_cache_on_miss(hit, &bootstrap);
    ensure!(
        !cache_population_paths.marker_path.exists(),
        "cache marker should remain absent on cache hit"
    );

    let miss = PostSetup {
        cache_config: &cache_config,
        cache_hit: false,
    };
    populate_cache_on_miss(miss, &bootstrap);
    ensure!(
        cache_population_paths.marker_path.exists(),
        "cache marker should be written on cache miss"
    );
    Ok(())
}
