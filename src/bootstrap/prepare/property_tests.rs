//! Property tests for pure bootstrap path resolution.

use super::settings_paths_from_settings;
use crate::PgEnvCfg;
use camino::Utf8PathBuf;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use std::fmt::Display;

#[derive(Debug, Clone)]
struct PathResolutionCase {
    install_segment: String,
    data_segment: String,
    uses_explicit_install: bool,
    uses_explicit_data: bool,
    repetition_count: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct PathSnapshot {
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    password_file: Utf8PathBuf,
    install_default: bool,
    data_default: bool,
}

fn segment_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,8}".prop_map(std::convert::identity)
}

fn path_resolution_case_strategy() -> impl Strategy<Value = PathResolutionCase> {
    (
        segment_strategy(),
        segment_strategy(),
        any::<bool>(),
        any::<bool>(),
        1usize..8,
    )
        .prop_map(
            |(
                install_segment,
                data_segment,
                uses_explicit_install,
                uses_explicit_data,
                repetition_count,
            )| PathResolutionCase {
                install_segment,
                data_segment,
                uses_explicit_install,
                uses_explicit_data,
                repetition_count,
            },
        )
}

fn testcase_failure<E: Display>(context: &str) -> impl FnOnce(E) -> TestCaseError + '_ {
    move |err| TestCaseError::fail(format!("{context}: {err}"))
}

fn utf8_path(path: std::path::PathBuf, context: &str) -> Result<Utf8PathBuf, TestCaseError> {
    Utf8PathBuf::from_path_buf(path).map_err(|bad_path| {
        TestCaseError::fail(format!("{context} is not UTF-8: {}", bad_path.display()))
    })
}

fn snapshot_for(
    settings: &mut postgresql_embedded::Settings,
    install_default: bool,
    data_default: bool,
) -> Result<PathSnapshot, TestCaseError> {
    let paths = settings_paths_from_settings(settings, install_default, data_default)
        .map_err(testcase_failure("resolve settings paths"))?;
    Ok(PathSnapshot {
        install_dir: paths.install_dir,
        data_dir: paths.data_dir,
        password_file: paths.password_file,
        install_default: paths.install_default,
        data_default: paths.data_default,
    })
}

proptest! {
    #[test]
    fn settings_path_resolution_converges_under_repetition(case in path_resolution_case_strategy()) {
        let sandbox = tempfile::tempdir()
            .map_err(|err| TestCaseError::fail(format!("create sandbox: {err}")))?;
        let install_dir = utf8_path(
            sandbox.path().join(&case.install_segment).join("install"),
            "install directory",
        )?;
        let data_dir = utf8_path(
            sandbox.path().join(&case.data_segment).join("data"),
            "data directory",
        )?;

        let cfg = PgEnvCfg {
            runtime_dir: case.uses_explicit_install.then_some(install_dir.clone()),
            data_dir: case.uses_explicit_data.then_some(data_dir.clone()),
            ..PgEnvCfg::default()
        };
        let mut settings = cfg
            .to_settings()
            .map_err(testcase_failure("convert configuration"))?;
        if !case.uses_explicit_install {
            settings.installation_dir = install_dir.clone().into_std_path_buf();
        }
        if !case.uses_explicit_data {
            settings.data_dir = data_dir.clone().into_std_path_buf();
        }

        let mut evolving_settings = settings;
        let expected = snapshot_for(
            &mut evolving_settings,
            !case.uses_explicit_install,
            !case.uses_explicit_data,
        )?;

        for _ in 0..case.repetition_count {
            let observed = snapshot_for(
                &mut evolving_settings,
                !case.uses_explicit_install,
                !case.uses_explicit_data,
            )?;
            prop_assert_eq!(&observed, &expected);
            prop_assert_eq!(observed.password_file, observed.install_dir.join(".pgpass"));
        }
    }
}
