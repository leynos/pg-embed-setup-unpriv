//! Unit tests for `pg_worker` data directory recovery and argument parsing.

use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::OsStrExt,
};

use pg_embedded_setup_unpriv::test_support::create_partial_data_dir;
use rstest::{fixture, rstest};
use tempfile::{TempDir, tempdir};

use super::*;

type R<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
type TempDataDirResult = R<(TempDir, Utf8PathBuf)>;

fn ensure(is_valid: bool, msg: &str) -> R { if is_valid { Ok(()) } else { Err(msg.into()) } }

#[fixture]
fn temp_data_dir() -> TempDataDirResult {
    let temp = tempdir()?;
    let p = Utf8PathBuf::from_path_buf(temp.path().join("data"))
        .map_err(|p| format!("not UTF-8: {}", p.display()))?;
    Ok((temp, p))
}

#[test]
fn rejects_extra_argument() -> R {
    let args = ["pg_worker", "setup", "/tmp/config.json", "unexpected"].map(OsString::from);
    let err = run_worker(args.into_iter()).err().ok_or("expected error")?;
    ensure(
        err.to_string().contains("unexpected extra argument"),
        "wrong err",
    )
}

#[test]
fn parse_args_rejects_non_utf8_config_path() -> R {
    let args = [
        OsString::from("pg_worker"),
        OsString::from("setup"),
        OsStr::from_bytes(&[0x80]).to_os_string(),
    ];
    match parse_args(args.into_iter()) {
        Err(WorkerError::InvalidArgs(m)) => ensure(
            m.to_lowercase().contains("utf-8") && m.contains("config"),
            "bad msg",
        ),
        o => Err(format!("expected InvalidArgs: {o:?}").into()),
    }
}

#[rstest]
fn valid_data_dir_detected(temp_data_dir: TempDataDirResult) -> R {
    let (_, p) = temp_data_dir?;
    fs::create_dir_all(p.join("global"))?;
    fs::write(p.join(PG_FILENODE_MAP_MARKER), "")?;
    ensure(has_valid_data_dir(&p)?, "should be valid")
}

#[rstest]
fn missing_dir_is_invalid(temp_data_dir: TempDataDirResult) -> R {
    ensure(!has_valid_data_dir(&temp_data_dir?.1)?, "should be invalid")
}

#[rstest]
fn dir_without_marker_is_invalid(temp_data_dir: TempDataDirResult) -> R {
    let (_, p) = temp_data_dir?;
    fs::create_dir_all(&p)?;
    ensure(!has_valid_data_dir(&p)?, "should be invalid")
}

#[rstest]
fn reset_removes_partial(temp_data_dir: TempDataDirResult) -> R {
    let (_, p) = temp_data_dir?;
    fs::create_dir_all(p.join("x"))?;
    reset_data_dir(&p)?;
    ensure(!p.exists(), "should be removed")
}

#[rstest]
fn reset_ok_for_missing(temp_data_dir: TempDataDirResult) -> R {
    let (_temp_dir, data_dir) = temp_data_dir?;
    reset_data_dir(&data_dir)
}

#[test]
fn reset_errors_on_root() -> R {
    let e = reset_data_dir(&Utf8PathBuf::from("/"))
        .err()
        .ok_or("expected err")?;
    ensure(
        e.to_string().to_lowercase().contains("root"),
        "should mention root",
    )
}

#[rstest]
fn recover_skips_nonexistent(temp_data_dir: TempDataDirResult) -> R {
    let (_, p) = temp_data_dir?;
    recover_invalid_data_dir(&p)?;
    ensure(!p.exists(), "should not exist")
}

#[rstest]
fn recover_skips_empty_dir(temp_data_dir: TempDataDirResult) -> R {
    let (_, p) = temp_data_dir?;
    fs::create_dir_all(&p)?;
    recover_invalid_data_dir(&p)?;
    ensure(p.exists(), "empty dir should remain")
}

/// Validates the recovery scenario from issue #80: a partial data directory
/// (missing `global/pg_filenode.map`) is detected as invalid and removed,
/// allowing fresh initialization to proceed.
#[rstest]
fn recover_removes_partial_initialization(temp_data_dir: TempDataDirResult) -> R {
    let (_, p) = temp_data_dir?;
    // Create a partial data directory using the shared helper
    create_partial_data_dir(p.as_std_path())?;

    // Verify the directory is detected as invalid (missing marker)
    ensure(!has_valid_data_dir(&p)?, "partial dir should be invalid")?;

    // Recovery should remove the partial directory
    recover_invalid_data_dir(&p)?;

    // After recovery, the directory should be gone
    ensure(!p.exists(), "partial dir should be removed by recovery")
}

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

#[rstest]
#[case("setup")]
#[case("start")]
#[case("stop")]
#[case("cleanup")]
#[case("cleanup-full")]
fn operation_parses_known_verbs(#[case] verb: &str) -> R {
    Operation::parse(OsStr::new(verb)).map_err(|e| format!("expected {verb} to parse: {e}"))?;
    Ok(())
}

#[test]
fn operation_parse_rejects_unknown_verb() -> R {
    let err = Operation::parse(OsStr::new("frobnicate"))
        .err()
        .ok_or("expected error")?;
    ensure(err.to_string().contains("unknown operation"), "wrong err")
}

#[test]
fn parse_args_requires_operation_and_config() -> R {
    let missing_op = parse_args(["pg_worker"].map(OsString::from).into_iter())
        .err()
        .ok_or("expected missing operation error")?;
    ensure(
        missing_op.to_string().contains("missing operation"),
        "wrong missing-op msg",
    )?;

    let missing_cfg = parse_args(["pg_worker", "setup"].map(OsString::from).into_iter())
        .err()
        .ok_or("expected missing config error")?;
    ensure(
        missing_cfg.to_string().contains("missing config path"),
        "wrong missing-cfg msg",
    )
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
// pgpass nested under the install dir yields the nested parent.
#[case(
    "/opt/pg/install",
    "/opt/pg/install/secrets/.pgpass",
    Some("/opt/pg/install/secrets")
)]
// pgpass directly in the install dir is treated as already covered.
#[case("/opt/pg/install", "/opt/pg/install/.pgpass", None)]
// pgpass outside the install dir is not a root to remove.
#[case("/opt/pg/install", "/elsewhere/.pgpass", None)]
// a parent-dir traversal is rejected defensively.
#[case("/opt/pg/install", "/opt/pg/install/../evil/.pgpass", None)]
// pgpass at the filesystem root has no meaningful parent to remove.
#[case("/opt/pg/install", "/.pgpass", None)]
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

#[rstest]
fn execute_cleanup_removes_existing_directories(temp_data_dir: TempDataDirResult) -> R {
    let (_temp, data) = temp_data_dir?;
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
    // SAFETY: nextest isolates each test in its own process.
    unsafe { std::env::set_var(unset_key, "pre-existing") };
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
    let cfg = write_config(&root, &settings, Vec::new())?;
    let payload = load_payload(&cfg).map_err(|e| e.to_string())?;
    let restored = payload
        .settings
        .into_settings()
        .map_err(|e| e.to_string())?;
    ensure(restored.data_dir == settings.data_dir, "data dir roundtrip")
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
