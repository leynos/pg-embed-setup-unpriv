//! Staging for the extension-hook lifecycle tests.
//!
//! The probe archive and its manifest, the worker-operation hook that records
//! the lifecycle, the two bootstrap builders, and the per-test case. Keeping
//! them here leaves `startup_extension_tests.rs` holding only assertions and
//! tests.

use std::sync::{Arc, Mutex};

use camino::Utf8Path;
use color_eyre::eyre::{Result, eyre};

use super::*;

/// Names of the fixture files the ordering tests install.
pub(super) const PROBE_FILES: [(&str, &[u8]); 2] = [
    ("lib/probe.so", b"module"),
    ("share/extension/probe.control", b"default_version = '1'\n"),
];

/// A worker-operation hook that creates the versioned tree on `Setup` and
/// records, at `Start`, whether the extension files were already present.
pub(super) struct OrderingHook {
    pub(super) operations: Arc<Mutex<Vec<String>>>,
    _hook_guard: crate::test_support::HookGuard,
}

pub(super) fn ordering_hook(install_root: &Utf8Path) -> Result<OrderingHook> {
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
pub(super) fn probe_request(base: &Utf8Path) -> Result<crate::extensions::ExtensionRequest> {
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
/// A root bootstrap carrying whatever extension request `build` produces.
///
/// The two callers differ only in that request, so the staging lives here.
pub(super) fn extension_bootstrap(
    paths: &RootSetupPaths,
    build: fn(&Utf8Path) -> Result<crate::extensions::ExtensionRequest>,
) -> Result<TestBootstrapSettings> {
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &paths.install_dir,
        &paths.data_dir,
        &paths.scoped_cache_home,
    )?;
    bootstrap.extensions = Some(build(&paths.install_dir)?);
    Ok(bootstrap)
}

/// A bootstrap whose extension request installs the probe archive.
pub(super) fn ordering_bootstrap(paths: &RootSetupPaths) -> Result<TestBootstrapSettings> {
    extension_bootstrap(paths, probe_request)
}
/// Everything one lifecycle test needs, staged once.
///
/// The six tests below differ only in which bootstrap they build and which
/// lifecycle they drive; staging the rest here keeps that difference the only
/// thing each test states.
pub(super) struct LifecycleCase {
    pub(super) hook: OrderingHook,
    pub(super) bootstrap: TestBootstrapSettings,
    pub(super) env_vars: Vec<(String, Option<String>)>,
    pub(super) cache_config: BinaryCacheConfig,
    pub(super) runtime: tokio::runtime::Runtime,
}

/// Stages a case whose bootstrap comes from `build`.
pub(super) fn lifecycle_case(
    paths: &RootSetupPaths,
    build: fn(&RootSetupPaths) -> Result<TestBootstrapSettings>,
) -> Result<LifecycleCase> {
    let hook = ordering_hook(&paths.install_dir)?;
    let bootstrap = build(paths)?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;
    Ok(LifecycleCase {
        hook,
        bootstrap,
        env_vars,
        cache_config,
        runtime,
    })
}
/// A request whose manifest cannot be parsed.
///
/// The hook fails at the first step, before it touches the cache or the
/// network, so the failure ordering is tested without either.
pub(super) fn failing_request(base: &Utf8Path) -> Result<crate::extensions::ExtensionRequest> {
    use crate::extensions::{ExtensionName, ExtensionRequest, ManifestSource};
    let manifest_path = base.join("broken-manifest.json");
    fs::write(manifest_path.as_std_path(), b"this is not a manifest")?;
    Ok(ExtensionRequest {
        names: vec![ExtensionName::new("probe").map_err(|err| eyre!("{err}"))?],
        manifest: ManifestSource::Path {
            path: manifest_path,
            sha256: None,
        },
        cache_dir: base.join("ext-cache"),
    })
}

/// The same bootstrap as [`ordering_bootstrap`], with a request that fails.
pub(super) fn failing_bootstrap(paths: &RootSetupPaths) -> Result<TestBootstrapSettings> {
    extension_bootstrap(paths, failing_request)
}

/// Whether another handle can take the setup lock in `install_root`.
pub(super) fn lock_is_free(install_root: &Utf8Path) -> bool {
    use fs4::FileExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(install_root.join(crate::cluster::setup_lock::SETUP_LOCK_FILE))
        .is_ok_and(|file| FileExt::try_lock(&file).is_ok())
}

/// Seeds the binary cache with a complete entry, so the lookup is a hit.
pub(super) fn seed_cache_hit(cache_dir: &Utf8Path) -> Result<()> {
    let entry = cache_dir.join(TEST_POSTGRES_VERSION);
    fs::create_dir_all(entry.join("bin").as_std_path())?;
    fs::write(entry.join(".complete").as_std_path(), b"")?;
    Ok(())
}

/// Replaces the bootstrap's manifest file with a FIFO and returns a thread
/// that reports whether the setup lock was held while the hook read it.
///
/// The hook opens the manifest only once it is installing, so a writer that
/// gets its open through is talking to a hook that is mid-install. The thread
/// samples the lock at that moment, then supplies the manifest so the install
/// carries on. The writer polls a non-blocking open, so a hook that never
/// reads the manifest fails the test rather than hanging it.
#[cfg(unix)]
pub(super) fn manifest_observing_the_lock(
    bootstrap: &TestBootstrapSettings,
    install_root: &Utf8Path,
) -> Result<std::thread::JoinHandle<Result<bool>>> {
    use std::{
        io::Write,
        os::unix::fs::OpenOptionsExt,
        time::{Duration, Instant},
    };

    let Some(crate::extensions::ExtensionRequest {
        manifest: crate::extensions::ManifestSource::Path { path: declared, .. },
        ..
    }) = bootstrap.extensions.as_ref()
    else {
        return Err(eyre!("the bootstrap must declare a path manifest"));
    };
    let path = declared.clone();
    let body = std::fs::read(path.as_std_path())?;
    std::fs::remove_file(path.as_std_path())?;
    nix::unistd::mkfifo(
        path.as_std_path(),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )?;
    let root = install_root.to_path_buf();
    Ok(std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_mins(1);
        let mut writer = loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path.as_std_path())
            {
                Ok(file) => break file,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(eyre!("the hook never read the manifest: {err}")),
            }
        };
        let held = !lock_is_free(&root);
        writer.write_all(&body)?;
        Ok(held)
    }))
}
