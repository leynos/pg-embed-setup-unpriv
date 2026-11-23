//! Behavioural tests for the preflight, driven through the resolver port.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs,
};

use camino::Utf8Path;
use rstest::rstest;
use tempfile::TempDir;

use super::{
    RuntimePreflightError,
    check::{ArchiveContext, Preflight},
    host::HostOs,
    marker,
    resolver::{LddResolver, ResolverUnavailable, SharedLibraryResolver, parse_missing_sonames},
};

/// A resolver whose answers are scripted by object file name.
#[derive(Default)]
struct FakeResolver {
    missing: HashMap<&'static str, Vec<&'static str>>,
    is_unavailable: bool,
    calls: Cell<usize>,
    seen: RefCell<Vec<String>>,
}

impl FakeResolver {
    fn missing(object: &'static str, sonames: &[&'static str]) -> Self {
        let mut fake = Self::default();
        fake.missing.insert(object, sonames.to_vec());
        fake
    }

    fn unavailable() -> Self {
        Self {
            is_unavailable: true,
            ..Self::default()
        }
    }
}

impl SharedLibraryResolver for FakeResolver {
    fn unresolved(&self, object: &Utf8Path) -> Result<Vec<String>, ResolverUnavailable> {
        self.calls.set(self.calls.get() + 1);
        self.seen.borrow_mut().push(object.to_string());
        if self.is_unavailable {
            return Err(ResolverUnavailable {
                reason: "ldd is not installed".to_owned(),
            });
        }
        let name = object.file_name().unwrap_or_default();
        Ok(self
            .missing
            .get(name)
            .map(|names| names.iter().map(|n| (*n).to_owned()).collect())
            .unwrap_or_default())
    }
}

fn ubuntu_26_04() -> HostOs {
    HostOs::parse(
        "PRETTY_NAME=\"Ubuntu 26.04.1 LTS\"\nID=ubuntu\nVERSION_ID=\"26.04\"\n",
        "x86_64",
    )
}

fn context(host: HostOs) -> ArchiveContext {
    ArchiveContext {
        postgresql_version: "17.11.0".to_owned(),
        archive_source: "https://github.com/theseus-rs/postgresql-binaries".to_owned(),
        host,
    }
}

/// A staged tree holding the three checked executables.
fn staged_tree() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).expect("create bin");
    for name in ["postgres", "initdb", "pg_ctl"] {
        fs::write(bin.join(name), b"").expect("write executable");
    }
    dir
}

#[test]
fn passes_and_records_when_every_library_resolves() {
    let staged = staged_tree();
    let resolver = FakeResolver::default();
    let preflight = Preflight::new(&resolver, context(ubuntu_26_04()));

    preflight.check_executables(staged.path()).expect("pass");

    assert_eq!(resolver.calls.get(), 3, "each executable is resolved once");
    assert!(staged.path().join(marker::MARKER_FILE).is_file());
}

#[test]
fn a_recorded_pass_is_not_checked_again() {
    let staged = staged_tree();
    let resolver = FakeResolver::default();
    let preflight = Preflight::new(&resolver, context(ubuntu_26_04()));
    preflight.check_executables(staged.path()).expect("first");
    let calls_after_first = resolver.calls.get();

    preflight.check_executables(staged.path()).expect("second");

    assert_eq!(resolver.calls.get(), calls_after_first);
}

#[test]
fn a_pass_recorded_for_another_host_is_checked_again() {
    let staged = staged_tree();
    let resolver = FakeResolver::default();
    let other_host = HostOs::parse("ID=ubuntu\nVERSION_ID=\"24.04\"\n", "x86_64");
    Preflight::new(&resolver, context(other_host))
        .check_executables(staged.path())
        .expect("other host");
    let calls_before = resolver.calls.get();

    Preflight::new(&resolver, context(ubuntu_26_04()))
        .check_executables(staged.path())
        .expect("this host");

    assert_eq!(resolver.calls.get(), calls_before + 3);
}

#[test]
fn missing_libraries_yield_an_actionable_typed_error() {
    let staged = staged_tree();
    let resolver = FakeResolver::missing("postgres", &["libxml2.so.2", "libicuuc.so.74"]);
    let preflight = Preflight::new(&resolver, context(ubuntu_26_04()));

    let err = preflight
        .check_executables(staged.path())
        .expect_err("unresolved");

    let RuntimePreflightError::Unresolved(found) = &err else {
        panic!("expected Unresolved, got {err:?}");
    };
    let [finding] = found.findings.as_slice() else {
        panic!("expected one finding, got {:?}", found.findings);
    };
    assert_eq!(finding.binary.file_name(), Some("postgres"));
    assert_eq!(finding.missing, ["libxml2.so.2", "libicuuc.so.74"]);
    let message = err.to_string();
    for needle in [
        "postgres",
        "libxml2.so.2",
        "libicuuc.so.74",
        "Ubuntu 26.04.1 LTS",
        "17.11.0",
        "https://github.com/theseus-rs/postgresql-binaries",
        "supported host image",
        "with_releases_url",
    ] {
        assert!(
            message.contains(needle),
            "{needle:?} missing from {message}"
        );
    }
    assert!(
        !staged.path().join(marker::MARKER_FILE).exists(),
        "a failure must not be remembered"
    );
}

#[test]
fn every_failing_binary_is_reported() {
    let staged = staged_tree();
    let mut resolver = FakeResolver::missing("postgres", &["libxml2.so.2"]);
    resolver.missing.insert("initdb", vec!["libxml2.so.2"]);
    let preflight = Preflight::new(&resolver, context(ubuntu_26_04()));

    let err = preflight
        .check_executables(staged.path())
        .expect_err("unresolved");

    let RuntimePreflightError::Unresolved(found) = err else {
        panic!("expected Unresolved");
    };
    assert_eq!(found.findings.len(), 2);
}

#[test]
fn an_unavailable_resolver_is_indeterminate_not_a_pass() {
    let staged = staged_tree();
    let resolver = FakeResolver::unavailable();
    let preflight = Preflight::new(&resolver, context(ubuntu_26_04()));

    let err = preflight
        .check_executables(staged.path())
        .expect_err("indeterminate");

    assert!(matches!(err, RuntimePreflightError::Indeterminate(_)));
    let message = err.to_string();
    assert!(message.contains("ldd is not installed"), "{message}");
    assert!(message.contains("libc-bin"), "{message}");
    assert!(message.contains("RuntimePreflight::Skip"), "{message}");
    assert!(!staged.path().join(marker::MARKER_FILE).exists());
}

#[rstest]
#[case::none("\tlibc.so.6 => /lib64/libc.so.6 (0x1)\n", &[])]
#[case::one("\tlibxml2.so.2 => not found\n\tlibc.so.6 => /lib/libc.so.6 (0x1)\n", &["libxml2.so.2"])]
#[case::duplicates_sorted("\tlibb.so.1 => not found\n\tliba.so.1 => not found\n\tlibb.so.1 => not found\n", &["liba.so.1", "libb.so.1"])]
#[case::empty("", &[])]
fn parses_ldd_output(#[case] output: &str, #[case] expected: &[&str]) {
    assert_eq!(parse_missing_sonames(output), expected);
}

#[test]
fn parses_os_release() {
    let host = ubuntu_26_04();
    assert_eq!(host.pretty_name, "Ubuntu 26.04.1 LTS");
    assert_eq!(host.fingerprint, "ubuntu-26.04-x86_64");
}

#[test]
fn an_absent_os_release_is_unknown_not_fatal() {
    let host = HostOs::from_os_release_file(Utf8Path::new("/nonexistent/os-release"));
    assert!(host.pretty_name.starts_with("unknown"), "{host:?}");
}

#[test]
fn a_host_without_the_resolver_program_is_unavailable() {
    let resolver = LddResolver::with_program("pg-embed-no-such-ldd");
    let err = resolver
        .unresolved(Utf8Path::new("/bin/sh"))
        .expect_err("no such program");
    assert!(
        err.reason.contains("pg-embed-no-such-ldd"),
        "{}",
        err.reason
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_real_resolver_finds_nothing_missing_for_a_system_binary() {
    let shell = Utf8Path::new("/bin/sh");
    let missing = LddResolver::new().unresolved(shell).expect("ldd runs");
    assert!(missing.is_empty(), "{missing:?}");
}

#[cfg(target_os = "linux")]
mod staged {
    //! The bootstrap-facing hooks, against a temporary staged tree.

    use std::path::Path;

    use camino::Utf8PathBuf;
    use color_eyre::eyre::eyre;

    use super::*;
    use crate::{
        BootstrapError,
        BootstrapErrorKind,
        ExecutionPrivileges,
        RuntimePreflight,
        TestBootstrapSettings,
        extensions::{ArchiveOrigin, ExtensionName, InstalledExtension, Sha256Hex},
        runtime_check::stage::{
            check_extension_libraries_with,
            check_staged_executables_with,
            explain_setup_failure_with,
        },
        test_support::dummy_settings,
    };

    fn utf8(path: &Path) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(path.to_path_buf()).expect("utf8")
    }

    fn bootstrap_for(staged: &TempDir) -> TestBootstrapSettings {
        let mut bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
        bootstrap.settings.installation_dir = staged.path().to_path_buf();
        bootstrap
    }

    #[test]
    fn missing_libraries_fail_with_the_incompatible_kind() {
        let staged = staged_tree();
        let bootstrap = bootstrap_for(&staged);
        let resolver = FakeResolver::missing("postgres", &["libxml2.so.2"]);

        let err = check_staged_executables_with(&resolver, &bootstrap).expect_err("fails");

        assert_eq!(err.kind(), BootstrapErrorKind::RuntimeIncompatible);
        assert!(err.to_string().contains("libxml2.so.2"));
    }

    #[test]
    fn an_unavailable_resolver_fails_with_the_indeterminate_kind() {
        let staged = staged_tree();
        let bootstrap = bootstrap_for(&staged);

        let err = check_staged_executables_with(&FakeResolver::unavailable(), &bootstrap)
            .expect_err("fails");

        assert_eq!(err.kind(), BootstrapErrorKind::RuntimeCheckIndeterminate);
    }

    #[test]
    fn skip_never_consults_the_resolver() {
        let staged = staged_tree();
        let mut bootstrap = bootstrap_for(&staged);
        bootstrap.runtime_preflight = RuntimePreflight::Skip;
        let resolver = FakeResolver::unavailable();

        check_staged_executables_with(&resolver, &bootstrap).expect("skipped");

        assert_eq!(resolver.calls.get(), 0);
    }

    #[test]
    fn nothing_staged_means_nothing_to_check() {
        let empty = tempfile::tempdir().expect("tempdir");
        let bootstrap = bootstrap_for(&empty);
        let resolver = FakeResolver::unavailable();

        check_staged_executables_with(&resolver, &bootstrap).expect("nothing staged");

        assert_eq!(resolver.calls.get(), 0);
    }

    #[test]
    fn a_failed_setup_is_explained_by_the_unresolved_library() {
        let staged = staged_tree();
        let bootstrap = bootstrap_for(&staged);
        let resolver = FakeResolver::missing("initdb", &["libxml2.so.2"]);
        let initdb_failure = BootstrapError::from(eyre!("initdb: error while loading libxml2"));

        let err = explain_setup_failure_with(&resolver, &bootstrap, initdb_failure);

        assert_eq!(err.kind(), BootstrapErrorKind::RuntimeIncompatible);
    }

    #[rstest]
    #[case::libraries_resolve(FakeResolver::default())]
    #[case::resolver_unavailable(FakeResolver::unavailable())]
    fn a_failed_setup_keeps_its_own_error_when_the_tree_is_not_to_blame(
        #[case] resolver: FakeResolver,
    ) {
        let staged = staged_tree();
        let bootstrap = bootstrap_for(&staged);
        let original = BootstrapError::from(eyre!("disk full"));

        let err = explain_setup_failure_with(&resolver, &bootstrap, original);

        assert_eq!(err.kind(), BootstrapErrorKind::Other);
        assert_eq!(err.to_string(), "disk full");
    }

    #[test]
    fn installed_extension_libraries_are_checked() {
        let staged = staged_tree();
        let mut bootstrap = bootstrap_for(&staged);
        let name = ExtensionName::new("vector").expect("name");
        bootstrap.installed_extensions = vec![InstalledExtension {
            name,
            version: "0.8.6".to_owned(),
            postgresql: "17.11.0".to_owned(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
            archive_sha256: Sha256Hex::parse(&"0".repeat(64)).expect("digest"),
            origin: ArchiveOrigin::Cached,
            files: vec![
                Utf8PathBuf::from("lib/vector.so"),
                Utf8PathBuf::from("share/extension/vector.control"),
            ],
        }];
        let resolver = FakeResolver::missing("vector.so", &["libgomp.so.1"]);

        let err = check_extension_libraries_with(&resolver, &bootstrap).expect_err("fails");

        assert_eq!(err.kind(), BootstrapErrorKind::RuntimeIncompatible);
        assert_eq!(
            *resolver.seen.borrow(),
            [utf8(&staged.path().join("lib/vector.so")).to_string()]
        );
    }
}
