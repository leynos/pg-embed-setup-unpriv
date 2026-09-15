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

    /// The unprivileged bootstrap adopts the stored password of an existing
    /// cluster when `PG_PASSWORD` is unset, and keeps an explicit one.
    #[rstest::rstest]
    #[case::adopts_stored(None, "stored-secret")]
    #[case::explicit_wins(Some("explicit-secret"), "explicit-secret")]
    fn bootstrap_unprivileged_reuses_stored_password(
        #[case] explicit: Option<&str>,
        #[case] expected: &str,
    ) {
        let runtime = tempdir().expect("runtime dir");
        let data = tempdir().expect("data dir");
        let runtime_dir =
            Utf8PathBuf::from_path_buf(runtime.path().to_path_buf()).expect("runtime dir utf8");
        let data_dir =
            Utf8PathBuf::from_path_buf(data.path().to_path_buf()).expect("data dir utf8");
        std::fs::write(data_dir.join("PG_VERSION"), "17\n").expect("cluster marker");
        std::fs::write(runtime_dir.join(".pgpass"), "stored-secret\n").expect("password file");

        let cfg = PgEnvCfg {
            runtime_dir: Some(runtime_dir.clone()),
            data_dir: Some(data_dir),
            password: explicit.map(str::to_owned),
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
        assert_eq!(prepared.settings.password, expected);
    }
}

#[path = "root_paths_tests.rs"]
mod root_paths;
