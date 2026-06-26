//! Property tests for cleanup lifecycle invariants.

use super::{
    cleanup_in_process, is_dangerous_cleanup_path, should_remove_data, should_remove_install,
};
use crate::CleanupMode;
use postgresql_embedded::Settings;
use proptest::prelude::*;
use proptest::test_runner::{TestCaseError, TestCaseResult};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
enum PathCase {
    Empty,
    Root,
    RelativeNested,
    AbsoluteNested,
    CurrentDirNested,
}

#[derive(Debug, Clone)]
struct DirectoryState {
    data: DirectoryEntryState,
    install: DirectoryEntryState,
}

#[derive(Debug, Clone, Copy)]
enum DirectoryEntryState {
    Missing,
    Empty,
    Marked,
}

impl DirectoryEntryState {
    const fn is_present(self) -> bool {
        matches!(self, Self::Empty | Self::Marked)
    }

    const fn has_marker(self) -> bool {
        matches!(self, Self::Marked)
    }
}

fn cleanup_mode_strategy() -> impl Strategy<Value = CleanupMode> {
    prop_oneof![
        Just(CleanupMode::DataOnly),
        Just(CleanupMode::Full),
        Just(CleanupMode::None),
    ]
}

fn path_case_strategy() -> impl Strategy<Value = PathCase> {
    prop_oneof![
        Just(PathCase::Empty),
        Just(PathCase::Root),
        Just(PathCase::RelativeNested),
        Just(PathCase::AbsoluteNested),
        Just(PathCase::CurrentDirNested),
    ]
}

fn directory_state_strategy() -> impl Strategy<Value = DirectoryState> {
    (
        directory_entry_state_strategy(),
        directory_entry_state_strategy(),
    )
        .prop_map(|(data, install)| DirectoryState { data, install })
}

fn directory_entry_state_strategy() -> impl Strategy<Value = DirectoryEntryState> {
    prop_oneof![
        Just(DirectoryEntryState::Missing),
        Just(DirectoryEntryState::Empty),
        Just(DirectoryEntryState::Marked),
    ]
}

fn path_for_case(path_case: PathCase) -> PathBuf {
    match path_case {
        PathCase::Empty => PathBuf::new(),
        PathCase::Root => PathBuf::from("/"),
        PathCase::RelativeNested => PathBuf::from("relative/nested"),
        PathCase::AbsoluteNested => PathBuf::from("/tmp/pg-embed-safe-nested"),
        PathCase::CurrentDirNested => PathBuf::from("./relative/nested"),
    }
}

fn expected_dangerous(path_case: PathCase) -> bool {
    matches!(path_case, PathCase::Empty | PathCase::Root)
}

fn io_failure(context: &str, err: &std::io::Error) -> TestCaseError {
    TestCaseError::fail(format!("{context}: {err}"))
}

fn create_dir_if_present(path: &Path, is_present: bool) -> TestCaseResult {
    if is_present {
        fs::create_dir_all(path).map_err(|err| io_failure("create directory", &err))?;
    }
    Ok(())
}

fn write_marker_if_present(path: &Path, dir_present: bool, marker_present: bool) -> TestCaseResult {
    if dir_present && marker_present {
        fs::write(path.join("marker"), b"marker")
            .map_err(|err| io_failure("write marker", &err))?;
    }
    Ok(())
}

fn seed_directories(settings: &Settings, state: &DirectoryState) -> TestCaseResult {
    create_dir_if_present(&settings.data_dir, state.data.is_present())?;
    create_dir_if_present(&settings.installation_dir, state.install.is_present())?;
    write_marker_if_present(
        &settings.data_dir,
        state.data.is_present(),
        state.data.has_marker(),
    )?;
    write_marker_if_present(
        &settings.installation_dir,
        state.install.is_present(),
        state.install.has_marker(),
    )?;
    Ok(())
}

fn assert_cleanup_postcondition(
    settings: &Settings,
    cleanup_mode: CleanupMode,
    expected_data_exists: &mut bool,
    expected_install_exists: &mut bool,
) -> TestCaseResult {
    match cleanup_mode {
        CleanupMode::DataOnly => {
            *expected_data_exists = false;
            prop_assert!(!settings.data_dir.exists());
            prop_assert_eq!(settings.installation_dir.exists(), *expected_install_exists);
        }
        CleanupMode::Full => {
            *expected_data_exists = false;
            *expected_install_exists = false;
            prop_assert!(!settings.data_dir.exists());
            prop_assert!(!settings.installation_dir.exists());
        }
        CleanupMode::None => {
            prop_assert_eq!(settings.data_dir.exists(), *expected_data_exists);
            prop_assert_eq!(settings.installation_dir.exists(), *expected_install_exists);
        }
    }
    prop_assert!(!is_dangerous_cleanup_path(&settings.data_dir));
    prop_assert!(!is_dangerous_cleanup_path(&settings.installation_dir));
    Ok(())
}

proptest! {
    #[test]
    fn cleanup_removal_predicates_are_consistent(mode in cleanup_mode_strategy()) {
        match mode {
            CleanupMode::DataOnly => {
                prop_assert!(should_remove_data(mode));
                prop_assert!(!should_remove_install(mode));
            }
            CleanupMode::Full => {
                prop_assert!(should_remove_data(mode));
                prop_assert!(should_remove_install(mode));
            }
            CleanupMode::None => {
                prop_assert!(!should_remove_data(mode));
                prop_assert!(!should_remove_install(mode));
            }
        }
    }

    #[test]
    fn dangerous_cleanup_path_detection_matches_path_shape(path_case in path_case_strategy()) {
        let path = path_for_case(path_case);

        prop_assert_eq!(is_dangerous_cleanup_path(&path), expected_dangerous(path_case));
    }

    #[test]
    fn cleanup_in_process_is_idempotent_for_generated_states(
        initial_state in directory_state_strategy(),
        operations in prop::collection::vec(cleanup_mode_strategy(), 1..16),
    ) {
        let sandbox = tempfile::tempdir()
            .map_err(|err| TestCaseError::fail(format!("create sandbox: {err}")))?;
        let settings = Settings {
            data_dir: sandbox.path().join("data"),
            installation_dir: sandbox.path().join("install"),
            password_file: sandbox.path().join("install/.pgpass"),
            ..Settings::default()
        };
        seed_directories(&settings, &initial_state)?;
        let mut expected_data_exists = initial_state.data.is_present();
        let mut expected_install_exists = initial_state.install.is_present();

        for cleanup_mode in operations {
            cleanup_in_process(cleanup_mode, &settings, "property-cleanup");
            cleanup_in_process(cleanup_mode, &settings, "property-cleanup");
            assert_cleanup_postcondition(
                &settings,
                cleanup_mode,
                &mut expected_data_exists,
                &mut expected_install_exists,
            )?;
        }
    }
}
