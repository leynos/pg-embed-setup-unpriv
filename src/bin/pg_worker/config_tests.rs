//! Configuration, payload, and cleanup tests for `pg_worker`.

use std::{ffi::OsString, fs};

use pg_embedded_setup_unpriv::test_support::scoped_env;
use rstest::rstest;
use tempfile::tempdir;

use super::*;

type R<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn ensure(is_valid: bool, msg: &str) -> R { if is_valid { Ok(()) } else { Err(msg.into()) } }

fn utf8(path: &std::path::Path) -> R<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path.to_path_buf())
        .map_err(|p| format!("non-UTF-8 temp path: {}", p.display()).into())
}

fn settings_with(install: &Utf8Path, pgpass: &Utf8Path, data: &Utf8Path) -> Settings {
    Settings {
        installation_dir: install.as_std_path().to_path_buf(),
        password_file: pgpass.as_std_path().to_path_buf(),
        data_dir: data.as_std_path().to_path_buf(),
        ..Settings::default()
    }
}

fn write_config(
    dir: &Utf8Path,
    settings: &Settings,
    env: Vec<(String, Option<String>)>,
) -> R<Utf8PathBuf> {
    let payload = WorkerPayload::new(settings, env)?;
    let bytes = serde_json::to_vec(&payload)?;
    let path = dir.join("config.json");
    fs::write(path.as_std_path(), bytes)?;
    Ok(path)
}

#[test]
fn extract_dirs_return_paths_from_settings() -> R {
    let install = Utf8Path::new("/opt/pg/install");
    let pgpass = Utf8Path::new("/opt/pg/install/.pgpass");
    let data = Utf8Path::new("/opt/pg/data");
    let settings = settings_with(install, pgpass, data);
    ensure(extract_data_dir(&settings)? == data, "data mismatch")?;
    ensure(
        extract_install_dir(&settings)? == install,
        "install mismatch",
    )
}

#[rstest]
#[case::nested_under_install(
    "/opt/pg/install",
    "/opt/pg/install/secrets/.pgpass",
    Some("/opt/pg/install/secrets")
)]
#[case::directly_in_install("/opt/pg/install", "/opt/pg/install/.pgpass", None)]
#[case::outside_install("/opt/pg/install", "/elsewhere/.pgpass", None)]
#[case::parent_dir_traversal("/opt/pg/install", "/opt/pg/install/../evil/.pgpass", None)]
#[case::filesystem_root("/opt/pg/install", "/.pgpass", None)]
fn extract_install_root_classifies_password_file(
    #[case] install: &str,
    #[case] pgpass: &str,
    #[case] expected: Option<&str>,
) -> R {
    let install_dir = Utf8Path::new(install);
    let settings = settings_with(
        install_dir,
        Utf8Path::new(pgpass),
        Utf8Path::new("/opt/pg/data"),
    );
    let resolved = extract_install_root(&settings, install_dir)?;
    let resolved_str = resolved.as_deref().map(Utf8Path::as_str);
    ensure(
        resolved_str == expected,
        "unexpected install-root classification",
    )
}

#[test]
fn execute_cleanup_removes_existing_directories() -> R {
    let temp = tempdir()?;
    let data = utf8(temp.path())?.join("data");
    fs::create_dir_all(data.join("base"))?;
    execute_cleanup(&data, None, None)?;
    ensure(!data.exists(), "data dir should be removed")
}

#[test]
fn execute_cleanup_removes_install_and_root() -> R {
    let temp = tempdir()?;
    let root = utf8(temp.path())?;
    let install = root.join("install");
    let install_root = root.join("install/secrets");
    let data = root.join("data");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&install_root)?;
    execute_cleanup(&data, Some(&install), Some(&install_root))?;
    ensure(!data.exists() && !install.exists(), "all dirs removed")
}

#[test]
fn is_dir_empty_reports_contents() -> R {
    let temp = tempdir()?;
    let root = utf8(temp.path())?;
    ensure(is_dir_empty(&root)?, "fresh dir should be empty")?;
    fs::write(root.join("file"), "x")?;
    ensure(!is_dir_empty(&root)?, "populated dir should not be empty")
}

#[test]
fn apply_worker_environment_sets_and_removes_variables() -> R {
    let set_key = "PG_WORKER_TEST_SET_VAR";
    let unset_key = "PG_WORKER_TEST_UNSET_VAR";
    // The shared guard seeds `unset_key`, restores both variables on drop, and
    // serialises env access, avoiding unguarded process-env mutation.
    let _env_guard = scoped_env(vec![
        (OsString::from(set_key), None),
        (
            OsString::from(unset_key),
            Some(OsString::from("pre-existing")),
        ),
    ]);
    let environment = vec![
        (set_key.to_owned(), Some(PlainSecret::from("applied"))),
        (unset_key.to_owned(), None),
    ];
    apply_worker_environment(&environment);
    ensure(
        std::env::var(set_key).as_deref() == Ok("applied"),
        "set var not applied",
    )?;
    ensure(std::env::var(unset_key).is_err(), "unset var not removed")
}

#[test]
fn build_runtime_constructs_current_thread_runtime() -> R {
    let runtime = build_runtime().map_err(|e| e.to_string())?;
    let answer = runtime.block_on(async { 21 + 21 });
    ensure(answer == 42, "runtime should execute futures")
}

#[rstest]
#[case("postmaster.pid does not exist", true)]
#[case("some other stop failure", false)]
fn stop_missing_pid_is_ok_matches_absent_pid(#[case] message: &str, #[case] expected: bool) -> R {
    let err = postgresql_embedded::Error::DatabaseStopError(message.to_owned());
    ensure(
        stop_missing_pid_is_ok(&err) == expected,
        "unexpected pid classification",
    )
}

#[test]
fn load_payload_roundtrips_written_config() -> R {
    let temp = tempdir()?;
    let root = utf8(temp.path())?;
    let settings = settings_with(
        &root.join("install"),
        &root.join("install/.pgpass"),
        &root.join("data"),
    );
    let env = vec![
        ("PG_WORKER_ENV_KEY".to_owned(), Some("env-value".to_owned())),
        ("PG_WORKER_ABSENT".to_owned(), None),
    ];
    let cfg = write_config(&root, &settings, env)?;
    let payload = load_payload(&cfg).map_err(|e| e.to_string())?;
    let restored = payload
        .settings
        .into_settings()
        .map_err(|e| e.to_string())?;
    ensure(
        restored.installation_dir == settings.installation_dir,
        "installation dir roundtrip",
    )?;
    ensure(
        restored.password_file == settings.password_file,
        "pgpass roundtrip",
    )?;
    ensure(restored.data_dir == settings.data_dir, "data dir roundtrip")?;

    let (present_key, present_value) = payload.environment.first().ok_or("missing env entry")?;
    ensure(
        present_key == "PG_WORKER_ENV_KEY"
            && present_value.as_ref().map(PlainSecret::expose) == Some("env-value"),
        "present env value should round-trip",
    )?;
    let (absent_key, absent_value) = payload.environment.get(1).ok_or("missing absent entry")?;
    ensure(
        absent_key == "PG_WORKER_ABSENT" && absent_value.is_none(),
        "absent env value should round-trip as None",
    )
}

#[test]
fn load_payload_rejects_missing_file() -> R {
    let temp = tempdir()?;
    let missing = utf8(temp.path())?.join("absent.json");
    let err = load_payload(&missing).err().ok_or("expected error")?;
    ensure(
        matches!(err, WorkerError::ConfigRead(_)),
        "expected ConfigRead error",
    )
}

#[test]
fn load_payload_rejects_invalid_json() -> R {
    let temp = tempdir()?;
    let root = utf8(temp.path())?;
    let cfg = root.join("config.json");
    fs::write(cfg.as_std_path(), b"not json")?;
    let err = load_payload(&cfg).err().ok_or("expected error")?;
    ensure(
        matches!(err, WorkerError::ConfigParse(_)),
        "expected ConfigParse error",
    )
}

#[test]
fn run_worker_cleanup_removes_data_dir_and_applies_environment() -> R {
    let temp = tempdir()?;
    let root = utf8(temp.path())?;
    let data = root.join("data");
    fs::create_dir_all(data.join("base"))?;
    let env_key = "PG_WORKER_CLEANUP_ENV";
    // Restore the worker-applied variable on drop and serialise env access.
    let _env_guard = scoped_env(vec![(OsString::from(env_key), None)]);
    let settings = settings_with(&root.join("install"), &root.join("install/.pgpass"), &data);
    let cfg = write_config(
        &root,
        &settings,
        vec![(env_key.to_owned(), Some("on".to_owned()))],
    )?;

    let args = ["pg_worker", "cleanup", cfg.as_str()].map(OsString::from);
    run_worker(args.into_iter()).map_err(|e| e.to_string())?;

    ensure(!data.exists(), "cleanup should remove the data dir")?;
    ensure(
        std::env::var(env_key).as_deref() == Ok("on"),
        "worker environment should be applied",
    )
}

#[test]
fn run_worker_cleanup_full_removes_install_tree() -> R {
    let temp = tempdir()?;
    let root = utf8(temp.path())?;
    let data = root.join("data");
    let install = root.join("install");
    let secrets = install.join("secrets");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&secrets)?;
    let settings = settings_with(&install, &secrets.join(".pgpass"), &data);
    let cfg = write_config(&root, &settings, Vec::new())?;

    let args = ["pg_worker", "cleanup-full", cfg.as_str()].map(OsString::from);
    run_worker(args.into_iter()).map_err(|e| e.to_string())?;

    ensure(
        !data.exists() && !install.exists(),
        "cleanup-full should remove data and install trees",
    )
}
