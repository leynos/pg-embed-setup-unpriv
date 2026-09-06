//! Tests for bootstrap preparation.

use super::*;

mod sanitized_settings {
    //! Tests for sanitized settings logging.
    use std::{collections::HashMap, time::Duration};

    use color_eyre::eyre::{Result, ensure};
    use postgresql_embedded::VersionReq;

    use super::log_sanitized_settings;
    use crate::test_support::capture_debug_logs;

    fn sample_settings() -> Result<postgresql_embedded::Settings> {
        let mut configuration = HashMap::new();
        configuration.insert("encoding".into(), "UTF8".into());
        configuration.insert("locale".into(), "en_US".into());

        Ok(postgresql_embedded::Settings {
            releases_url: "https://example.invalid/releases".into(),
            version: VersionReq::parse("=17.1.0")?,
            installation_dir: "/tmp/sanitized/install".into(),
            password_file: "/tmp/sanitized/.pgpass".into(),
            data_dir: "/tmp/sanitized/data".into(),
            host: "127.0.0.1".into(),
            port: 15_432,
            username: "integration".into(),
            password: "super-secret-pass".into(),
            temporary: false,
            timeout: Some(Duration::from_secs(12)),
            configuration,
            trust_installation_dir: true,
            socket_dir: Some("/tmp/sanitized/socket".into()),
        })
    }

    #[test]
    fn sanitized_settings_log_redacts_passwords() -> Result<()> {
        let settings = sample_settings()?;
        let (logs, ()) = capture_debug_logs(|| log_sanitized_settings(&settings));
        let joined = logs.join("\n");

        ensure!(
            joined.contains("prepared postgres settings"),
            "expected settings log, got {joined}"
        );
        ensure!(
            joined.contains("port=15432"),
            "expected port to appear in logs, got {joined}"
        );
        ensure!(
            joined.contains("installation_dir=/tmp/sanitized/install"),
            "expected installation dir to appear in logs, got {joined}"
        );
        ensure!(
            joined.contains("data_dir=/tmp/sanitized/data"),
            "expected data dir to appear in logs, got {joined}"
        );
        ensure!(
            joined.contains("password=") && joined.contains("<redacted>"),
            "expected redacted password marker, got {joined}"
        );
        ensure!(
            joined.contains("=17.1.0"),
            "expected version requirement to appear, got {joined}"
        );
        ensure!(
            joined.contains("configuration_keys=[\"encoding\", \"locale\"]"),
            "expected configuration keys to be logged, got {joined}"
        );
        ensure!(
            !joined.contains("super-secret-pass"),
            "log output leaked the password: {joined}"
        );

        Ok(())
    }
}

mod behaviour_tests {
    //! Behavioural tests for bootstrap preparation.
    use std::ffi::OsString;

    use tempfile::tempdir;

    use super::*;
    use crate::test_support::scoped_env;

    #[test]
    fn bootstrap_unprivileged_sets_up_directories() {
        let runtime = tempdir().expect("runtime dir");
        let data = tempdir().expect("data dir");
        let runtime_dir =
            Utf8PathBuf::from_path_buf(runtime.path().to_path_buf()).expect("runtime dir utf8");
        let data_dir =
            Utf8PathBuf::from_path_buf(data.path().to_path_buf()).expect("data dir utf8");

        let cfg = PgEnvCfg {
            runtime_dir: Some(runtime_dir.clone()),
            data_dir: Some(data_dir.clone()),
            ..PgEnvCfg::default()
        };
        let settings = cfg.to_settings().expect("settings");

        let _guard = scoped_env(vec![
            (
                OsString::from("TZDIR"),
                Some(OsString::from(runtime_dir.as_str())),
            ),
            (OsString::from("TZ"), Some(OsString::from("UTC"))),
        ]);
        let prepared = bootstrap_unprivileged(settings, &cfg).expect("bootstrap");

        assert_eq!(prepared.environment.home, runtime_dir);
        assert!(prepared.environment.xdg_cache_home.exists());
        assert!(prepared.environment.xdg_runtime_dir.exists());
        assert_eq!(
            prepared.environment.pgpass_file,
            runtime_dir.join(".pgpass")
        );
        let observed_install =
            Utf8PathBuf::from_path_buf(prepared.settings.installation_dir.clone())
                .expect("installation dir utf8");
        let observed_data =
            Utf8PathBuf::from_path_buf(prepared.settings.data_dir.clone()).expect("data dir utf8");
        assert_eq!(observed_install, runtime_dir);
        assert_eq!(observed_data, data_dir);
    }
}

#[cfg(not(all(unix, privileged_unix_platform)))]
mod portable_root_tests {
    //! Resolver coverage for platforms without a per-user default tree.
    use camino::Utf8PathBuf;
    use rstest::rstest;

    use super::*;

    /// Resolves the settings paths for `cfg` on the current platform.
    fn resolve(cfg: &PgEnvCfg) -> BootstrapResult<SettingsPaths> {
        let mut settings = cfg.to_settings()?;
        resolve_settings_paths_for_current_user(&mut settings, cfg)
    }

    /// Without `PG_EMBED_ROOT` the `Settings` defaults stand and neither
    /// leaf is reported as derived.
    #[test]
    fn settings_defaults_are_kept_without_embed_root() {
        let cfg = PgEnvCfg::default();
        let mut settings = cfg.to_settings().expect("settings");
        let expected_install = settings.installation_dir.clone();
        let expected_data = settings.data_dir.clone();
        let paths =
            resolve_settings_paths_for_current_user(&mut settings, &cfg).expect("settings paths");
        assert_eq!(paths.install_dir.as_std_path(), expected_install);
        assert_eq!(paths.data_dir.as_std_path(), expected_data);
        assert!(!paths.install_default && !paths.data_default);
    }

    /// `PG_EMBED_ROOT` derives both leaves on every platform.
    #[rstest]
    #[case::install(|paths: &SettingsPaths| (paths.install_dir.to_string(), paths.install_default), "install")]
    #[case::data(|paths: &SettingsPaths| (paths.data_dir.to_string(), paths.data_default), "data")]
    fn embed_root_derives_both_leaves(
        #[case] pick: fn(&SettingsPaths) -> (String, bool),
        #[case] leaf: &str,
    ) {
        let root = Utf8PathBuf::from_path_buf(std::env::temp_dir().join("pg-embed-root"))
            .expect("temp dir is UTF-8");
        let cfg = PgEnvCfg {
            embed_root: Some(root.clone()),
            ..PgEnvCfg::default()
        };
        let paths = resolve(&cfg).expect("settings paths");
        assert_eq!(pick(&paths), (root.join(leaf).to_string(), true));
    }

    /// An explicit leaf still wins over the root-derived default.
    #[rstest]
    #[case::runtime_dir(true)]
    #[case::data_dir(false)]
    fn explicit_leaf_wins_over_embed_root(#[case] runtime: bool) {
        let root = Utf8PathBuf::from_path_buf(std::env::temp_dir().join("pg-embed-root"))
            .expect("temp dir is UTF-8");
        let explicit = Utf8PathBuf::from_path_buf(std::env::temp_dir().join("elsewhere"))
            .expect("temp dir is UTF-8");
        let cfg = PgEnvCfg {
            embed_root: Some(root),
            runtime_dir: runtime.then(|| explicit.clone()),
            data_dir: (!runtime).then(|| explicit.clone()),
            ..PgEnvCfg::default()
        };
        let paths = resolve(&cfg).expect("settings paths");
        let (observed, derived) = if runtime {
            (paths.install_dir.clone(), paths.install_default)
        } else {
            (paths.data_dir.clone(), paths.data_default)
        };
        assert_eq!((observed, derived), (explicit, false));
    }
}

#[cfg(all(unix, privileged_unix_platform))]
mod unix_tests {
    //! Unix-specific permission tests for bootstrap preparation.
    use std::{
        os::unix::fs::{MetadataExt, PermissionsExt},
        sync::mpsc,
        thread,
        time::Duration,
    };

    use nix::{
        sys::stat::Mode,
        unistd::{Uid, User, geteuid},
    };
    use rstest::rstest;
    use tempfile::tempdir;

    use super::{unix_user::ensure_pgpass_for_user, *};
    use crate::privileges::default_paths_for;

    #[test]
    fn ensure_settings_paths_applies_defaults() {
        let cfg = PgEnvCfg::default();
        let mut settings = cfg.to_settings().expect("default config should convert");
        let uid = Uid::from_raw(9999);

        let paths =
            resolve_settings_paths_for_uid(&mut settings, &cfg, uid).expect("settings paths");
        let (expected_install, expected_data) = default_paths_for(uid);

        assert_eq!(paths.install_dir, expected_install);
        assert_eq!(paths.data_dir, expected_data);
        assert_eq!(paths.password_file, expected_install.join(".pgpass"));
        assert!(paths.install_default);
        assert!(paths.data_default);
    }

    /// An explicit root yields `install` and `data` leaves directly beneath it.
    #[test]
    fn default_paths_under_derive_leaves_from_root() {
        let (install, data) = default_paths_under(Utf8Path::new("/srv/project/pg"));
        assert_eq!(install.as_str(), "/srv/project/pg/install");
        assert_eq!(data.as_str(), "/srv/project/pg/data");
    }

    /// Resolves the default paths for a configuration under a fixed uid.
    fn resolve(cfg: &PgEnvCfg) -> BootstrapResult<SettingsPaths> {
        let mut settings = cfg.to_settings()?;
        resolve_settings_paths_for_uid(&mut settings, cfg, Uid::from_raw(9999))
    }

    /// `PG_EMBED_ROOT` replaces the per-user base for both derived leaves,
    /// which still count as defaults.
    #[rstest]
    #[case::install("/srv/project/pg/install", |paths: &SettingsPaths| (paths.install_dir.to_string(), paths.install_default))]
    #[case::data("/srv/project/pg/data", |paths: &SettingsPaths| (paths.data_dir.to_string(), paths.data_default))]
    fn ensure_settings_paths_honours_embed_root(
        #[case] expected: &str,
        #[case] pick: fn(&SettingsPaths) -> (String, bool),
    ) {
        let cfg = PgEnvCfg {
            embed_root: Some(Utf8PathBuf::from("/srv/project/pg")),
            ..PgEnvCfg::default()
        };
        let paths = resolve(&cfg).expect("settings paths");
        assert_eq!(pick(&paths), (expected.to_owned(), true));
    }

    /// An explicit `PG_RUNTIME_DIR` wins over the root-derived install leaf
    /// while the data leaf is still derived from the root.
    #[test]
    fn explicit_runtime_dir_wins_over_embed_root() {
        let cfg = PgEnvCfg {
            embed_root: Some(Utf8PathBuf::from("/srv/project/pg")),
            runtime_dir: Some(Utf8PathBuf::from("/elsewhere/install")),
            ..PgEnvCfg::default()
        };
        let paths = resolve(&cfg).expect("settings paths");
        assert_eq!(
            (
                paths.install_dir.as_str(),
                paths.install_default,
                paths.data_dir.as_str(),
                paths.data_default
            ),
            ("/elsewhere/install", false, "/srv/project/pg/data", true)
        );
    }

    /// An explicit leaf override wins over the root-derived default.
    #[test]
    fn explicit_leaves_win_over_embed_root() {
        let cfg = PgEnvCfg {
            embed_root: Some(Utf8PathBuf::from("/srv/project/pg")),
            data_dir: Some(Utf8PathBuf::from("/elsewhere/data")),
            ..PgEnvCfg::default()
        };
        let paths = resolve(&cfg).expect("settings paths");
        assert_eq!(
            (
                paths.install_dir.as_str(),
                paths.data_dir.as_str(),
                paths.data_default
            ),
            ("/srv/project/pg/install", "/elsewhere/data", false)
        );
    }

    #[test]
    fn ensure_settings_paths_respects_user_provided_dirs() {
        let sandbox = tempdir().expect("settings sandbox");
        let install_path = sandbox.path().join("install");
        let data_path = sandbox.path().join("data");
        let install_dir =
            Utf8PathBuf::from_path_buf(install_path).expect("install dir should be utf8");
        let data_dir = Utf8PathBuf::from_path_buf(data_path).expect("data dir should be utf8");
        let cfg = PgEnvCfg {
            runtime_dir: Some(install_dir.clone()),
            data_dir: Some(data_dir.clone()),
            ..PgEnvCfg::default()
        };
        let mut settings = cfg.to_settings().expect("custom config should convert");
        let uid = Uid::from_raw(4242);

        let paths =
            resolve_settings_paths_for_uid(&mut settings, &cfg, uid).expect("settings paths");

        assert_eq!(paths.install_dir, install_dir);
        assert_eq!(paths.data_dir, data_dir);
        assert_eq!(paths.password_file, paths.install_dir.join(".pgpass"));
        assert!(!paths.install_default);
        assert!(!paths.data_default);
    }

    #[test]
    fn ensure_pgpass_for_user_sets_permissions_and_owner() {
        let sandbox = tempdir().expect("pgpass sandbox");
        let path = sandbox.path().join(".pgpass");
        std::fs::write(&path, b"test").expect("write pgpass");
        let mut perms = std::fs::metadata(&path)
            .expect("pgpass metadata")
            .permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).expect("set initial pgpass permissions");

        let user = User::from_uid(geteuid())
            .expect("resolve current user")
            .expect("current user should exist");
        let utf8_path = Utf8PathBuf::from_path_buf(path).expect("pgpass path utf8");

        ensure_pgpass_for_user(&utf8_path, &user).expect("ensure pgpass for user");

        let metadata = std::fs::metadata(utf8_path.as_std_path()).expect("pgpass metadata");
        let observed_mode = metadata.permissions().mode() & 0o777;
        assert_eq!(observed_mode, PGPASS_MODE);
        assert_eq!(metadata.uid(), user.uid.as_raw());
        assert_eq!(metadata.gid(), user.gid.as_raw());
    }

    #[test]
    fn ensure_pgpass_for_user_rejects_fifo_without_blocking() {
        let sandbox = tempdir().expect("pgpass sandbox");
        let path = sandbox.path().join(".pgpass");
        nix::unistd::mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).expect("create pgpass FIFO");
        let user = User::from_uid(geteuid())
            .expect("resolve current user")
            .expect("current user should exist");
        let utf8_path = Utf8PathBuf::from_path_buf(path).expect("pgpass path utf8");
        let (sender, receiver) = mpsc::channel();

        let worker = thread::spawn(move || {
            sender
                .send(ensure_pgpass_for_user(&utf8_path, &user))
                .expect("send pgpass preparation result");
        });
        let result = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("PGPASSFILE FIFO validation must not block without a writer");
        worker.join().expect("pgpass preparation worker");

        let err = result.expect_err("PGPASSFILE FIFO validation should fail");
        assert!(
            err.to_string().contains("must reference a regular file"),
            "expected regular-file rejection, got: {err}"
        );
    }
}
