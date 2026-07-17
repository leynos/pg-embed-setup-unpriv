//! Capability-aware filesystem helpers for tests, mirroring the public `fs`
//! API while exposing ambient operations for scaffolding.

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::fs::Dir;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use cap_std::{ambient_authority, fs::Metadata};
use color_eyre::eyre::Result;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use color_eyre::eyre::{Context, Report};

use crate::fs;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use crate::{ExecutionPrivileges, detect_execution_privileges};

/// Opens the ambient directory containing `path` and returns its relative component.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::ambient_dir_and_path;
/// # fn main() -> Result<()> {
/// let (_dir, relative) = ambient_dir_and_path(Utf8Path::new("."))?;
/// assert_eq!(relative.as_str(), ".");
///
/// let (_root, root_rel) = ambient_dir_and_path(Utf8Path::new("/"))?;
/// assert!(root_rel.as_str().is_empty());
/// # Ok(())
/// # }
/// ```
pub fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    fs::ambient_dir_and_path(path)
}

/// Ensures the provided directory exists, creating intermediate components when missing.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::ensure_dir_exists;
/// # fn main() -> Result<()> {
/// ensure_dir_exists(Utf8Path::new("./target/tmp/cache"))?;
/// # Ok(())
/// # }
/// ```
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn ensure_dir_exists(path: &Utf8Path) -> Result<()> { fs::ensure_dir_exists(path) }

/// Applies POSIX permissions to the provided path when it already exists.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::set_permissions;
/// # fn main() -> Result<()> {
/// set_permissions(Utf8Path::new("./target/tmp/cache"), 0o755)?;
/// # Ok(())
/// # }
/// ```
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> { fs::set_permissions(path, mode) }

/// Retrieves metadata for the provided path using capability APIs.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn metadata(path: &Utf8Path) -> std::io::Result<Metadata> {
    let (dir, relative) = ambient_dir_and_path(path).map_err(std::io::Error::other)?;
    if relative.as_str().is_empty() {
        dir.dir_metadata()
    } else {
        dir.metadata(relative.as_std_path())
    }
}

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
fn temp_root_dir() -> Result<Utf8PathBuf> {
    let privileges = detect_execution_privileges();
    let base = match env_temp_base(privileges)? {
        Some(path) => path,
        None => default_temp_base(privileges)?,
    };

    let root = base.join("pg-embedded-setup-unpriv-tmp");
    std::fs::create_dir_all(root.as_std_path())
        .with_context(|| format!("create temp root at {root}"))?;
    if matches!(privileges, ExecutionPrivileges::Root) {
        fs::set_permissions(&root, 0o777)
            .with_context(|| format!("set temp root permissions for {root}"))?;
    }
    Ok(root)
}

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
fn env_temp_base(privileges: ExecutionPrivileges) -> Result<Option<Utf8PathBuf>> {
    let vars: &[&str] = match privileges {
        ExecutionPrivileges::Root => &["PG_EMBEDDED_TEST_TMPDIR"],
        ExecutionPrivileges::Unprivileged => &["PG_EMBEDDED_TEST_TMPDIR", "CARGO_TARGET_DIR"],
    };

    for var in vars {
        if let Some(path) = resolve_env_path(var)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
fn resolve_env_path(var: &str) -> Result<Option<Utf8PathBuf>> {
    std::env::var_os(var)
        .map(|path| {
            Utf8PathBuf::try_from(std::path::PathBuf::from(path))
                .map_err(|_| Report::msg(format!("{var} is not valid UTF-8")))
        })
        .transpose()
}

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
fn default_temp_base(privileges: ExecutionPrivileges) -> Result<Utf8PathBuf> {
    match privileges {
        ExecutionPrivileges::Root => {
            #[cfg(unix)]
            {
                Ok(Utf8PathBuf::from("/var/tmp"))
            }

            #[cfg(not(unix))]
            {
                target_temp_base()
            }
        }
        ExecutionPrivileges::Unprivileged => target_temp_base(),
    }
}

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
fn target_temp_base() -> Result<Utf8PathBuf> {
    let cwd_path = std::env::current_dir().context("resolve current directory")?;
    let cwd = Utf8PathBuf::try_from(cwd_path)
        .map_err(|_| Report::msg("current directory is not valid UTF-8"))?;
    Ok(cwd.join("target"))
}

/// Capability-aware temporary directory that exposes both a [`Dir`] handle and the UTF-8 path.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
#[derive(Debug)]
pub struct CapabilityTempDir {
    dir: Option<Dir>,
    path: Utf8PathBuf,
}

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
impl CapabilityTempDir {
    /// Creates a new temporary directory rooted under the test temp location.
    ///
    /// Uses `PG_EMBEDDED_TEST_TMPDIR` when set. Unprivileged tests prefer
    /// `CARGO_TARGET_DIR` (or `./target`) to avoid exhausting shared `/tmp`
    /// space; privileged tests default to `/var/tmp` so the unprivileged worker
    /// can access the directory tree.
    pub fn new(prefix: &str) -> Result<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let temp_root = temp_root_dir()?;
        let ambient = Dir::open_ambient_dir(temp_root.as_std_path(), ambient_authority())
            .context("open ambient temp directory")?;

        let pid = std::process::id();
        let epoch_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();

        for attempt in 0..32 {
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!("{}-{}-{}-{}", prefix, pid, epoch_ns, counter + attempt);
            match ambient.create_dir(&name) {
                Ok(()) => {
                    let dir = ambient.open_dir(&name).context("open capability tempdir")?;
                    let path = temp_root.join(&name);
                    return Ok(Self {
                        dir: Some(dir),
                        path,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err).with_context(|| format!("create capability tempdir {name}"));
                }
            }
        }

        Err(Report::msg(
            "exhausted attempts creating capability tempdir",
        ))
    }

    /// Returns the UTF-8 path to the temporary directory.
    #[must_use]
    pub fn path(&self) -> &Utf8Path { &self.path }

    fn remove_dir(dir: Dir, path: &Utf8Path) {
        if let Err(err) = dir.remove_open_dir_all() {
            tracing::warn!("SKIP-CAP-TEMPDIR: failed to remove {}: {err}", path);
        }
    }
}

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
impl Drop for CapabilityTempDir {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            Self::remove_dir(dir, &self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for capability-aware filesystem test helpers.

    use super::*;

    #[test]
    fn capability_temp_dir_creates_and_cleans_up() {
        let temp = CapabilityTempDir::new("cov-cap").expect("create capability tempdir");
        let path = temp.path().to_path_buf();
        assert!(path.exists(), "temp dir should exist while held");
        assert!(
            metadata(&path).expect("stat temp dir").is_dir(),
            "temp dir metadata should report a directory"
        );
        drop(temp);
        assert!(!path.exists(), "temp dir should be removed on drop");
    }

    #[test]
    fn ensure_dir_and_set_permissions_wrappers_operate() {
        let temp = CapabilityTempDir::new("cov-wrap").expect("create capability tempdir");
        let nested = temp.path().join("nested/child");
        ensure_dir_exists(&nested).expect("ensure nested dir");
        assert!(nested.exists(), "nested dir should be created");
        set_permissions(&nested, 0o750).expect("apply permissions");

        // `set_permissions` applies an absolute mode (not umask-masked), so the
        // resulting directory must carry exactly the requested bits. The
        // non-Unix wrapper is a documented no-op, so only assert on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(nested.as_std_path())
                .expect("stat nested dir")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o750, "set_permissions should apply the exact mode");
        }
    }

    #[test]
    fn resolve_env_path_reads_present_and_absent_variables() {
        let _guard = crate::env::ScopedEnv::apply(&[(
            String::from("PG_EMBEDDED_TEST_TMPDIR"),
            Some(String::from("/tmp/cov-env-path")),
        )]);
        let resolved = resolve_env_path("PG_EMBEDDED_TEST_TMPDIR")
            .expect("resolve present var")
            .expect("present var yields a path");
        assert_eq!(resolved.as_str(), "/tmp/cov-env-path");

        assert!(
            resolve_env_path("PG_EMBEDDED_TEST_DEFINITELY_UNSET")
                .expect("resolve absent var")
                .is_none(),
            "absent var should resolve to None"
        );
    }
}
