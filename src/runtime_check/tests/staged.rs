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

    let err =
        check_staged_executables_with(&FakeResolver::unavailable(), &bootstrap).expect_err("fails");

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
fn a_failed_setup_is_explained_even_when_a_pass_was_recorded() {
    let staged = staged_tree();
    let bootstrap = bootstrap_for(&staged);
    check_staged_executables_with(&FakeResolver::default(), &bootstrap).expect("first pass");
    assert!(staged.path().join(marker::MARKER_FILE).is_file());
    let resolver = FakeResolver::missing("postgres", &["libxml2.so.2"]);
    let initdb_failure = BootstrapError::from(eyre!("initdb failed"));

    let err = explain_setup_failure_with(&resolver, &bootstrap, initdb_failure);

    assert_eq!(err.kind(), BootstrapErrorKind::RuntimeIncompatible);
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

#[test]
fn a_failed_setup_keeps_its_own_error_when_the_libraries_resolve() {
    let staged = staged_tree();
    let bootstrap = bootstrap_for(&staged);
    let original = BootstrapError::from(eyre!("disk full"));

    let err = explain_setup_failure_with(&FakeResolver::default(), &bootstrap, original);

    assert_eq!(err.kind(), BootstrapErrorKind::Other);
    assert_eq!(err.to_string(), "disk full");
}

#[test]
fn a_failed_setup_keeps_its_kind_and_notes_an_unavailable_resolver() {
    let staged = staged_tree();
    let bootstrap = bootstrap_for(&staged);
    let original = BootstrapError::from(eyre!("disk full"));

    let err = explain_setup_failure_with(&FakeResolver::unavailable(), &bootstrap, original);

    assert_eq!(err.kind(), BootstrapErrorKind::Other);
    let message = err.to_string();
    assert!(message.contains("disk full"), "{message}");
    assert!(
        message.contains("runtime preflight unavailable: ldd is not installed"),
        "{message}"
    );
}

/// An installed `vector` extension that owns exactly `files`.
fn installed_extension_with(files: &[&str]) -> InstalledExtension {
    InstalledExtension {
        name: ExtensionName::new("vector").expect("name"),
        version: "0.8.6".to_owned(),
        postgresql: "17.11.0".to_owned(),
        target: "x86_64-unknown-linux-gnu".to_owned(),
        archive_sha256: Sha256Hex::parse(&"0".repeat(64)).expect("digest"),
        origin: ArchiveOrigin::Cached,
        files: files.iter().map(Utf8PathBuf::from).collect(),
    }
}

#[test]
fn credentials_in_the_release_url_reach_neither_the_error_nor_the_marker() {
    let staged = staged_tree();
    let mut bootstrap = bootstrap_for(&staged);
    bootstrap.settings.releases_url =
        "https://user:hunter2@example.com/pg?sig=abc123#frag".to_owned();
    let failing = FakeResolver::missing("postgres", &["libxml2.so.2"]);

    let err = check_staged_executables_with(&failing, &bootstrap).expect_err("fails");

    let message = err.to_string();
    assert!(message.contains("https://example.com/pg"), "{message}");
    assert_no_secrets(&message);
    check_staged_executables_with(&FakeResolver::default(), &bootstrap).expect("passes");
    let marker =
        fs::read_to_string(staged.path().join(crate::runtime_check::MARKER_FILE)).expect("marker");
    assert_no_secrets(&marker);
}

/// Fails if `text` carries any part of the credentialed release URL.
fn assert_no_secrets(text: &str) {
    for secret in ["hunter2", "abc123", "frag"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
}

#[test]
fn versioned_shared_libraries_are_checked_and_other_files_are_not() {
    let staged = staged_tree();
    let mut bootstrap = bootstrap_for(&staged);
    let files = [
        "lib/vector.so",
        "lib/example.so.1",
        "lib/example.so.1.2",
        "share/extension/vector.control",
        "share/extension/vector--0.8.sql",
    ];
    bootstrap.installed_extensions = vec![installed_extension_with(&files)];
    let resolver = FakeResolver::default();

    check_extension_libraries_with(&resolver, &bootstrap).expect("passes");

    let seen: Vec<String> = resolver
        .seen
        .borrow()
        .iter()
        .map(|path| path.rsplit('/').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(seen, ["vector.so", "example.so.1", "example.so.1.2"]);
}

#[test]
fn a_non_elf_file_is_unavailable_not_static() {
    let dir = tempfile::tempdir().expect("tempdir");
    let text = dir.path().join("not-elf.so");
    fs::write(&text, b"plain text").expect("write");
    let object = Utf8PathBuf::from_path_buf(text).expect("utf8");

    let outcome = LddResolver::new().unresolved(&object);

    assert!(outcome.is_err(), "{outcome:?}");
}

#[test]
fn a_truncated_elf_is_unavailable_not_static() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cut = dir.path().join("cut.so");
    fs::write(&cut, b"\x7fELF\x02\x01\x01").expect("write");
    let object = Utf8PathBuf::from_path_buf(cut).expect("utf8");

    let outcome = LddResolver::new().unresolved(&object);

    assert!(outcome.is_err(), "{outcome:?}");
}

#[test]
fn a_system_ldconfig_resolves_whether_it_is_static_or_dynamic() {
    // ldconfig is a statically linked ELF on many distributions, which is the
    // case the static fallback exists for. Some distributions ship a shell
    // wrapper instead, which is correctly not a pass, so only an ELF counts.
    let Some(path) = ["/usr/sbin/ldconfig", "/sbin/ldconfig"]
        .into_iter()
        .find(|path| fs::read(path).is_ok_and(|bytes| bytes.starts_with(b"\x7fELF")))
    else {
        return;
    };

    let outcome = LddResolver::new().unresolved(Utf8Path::new(path));

    assert_eq!(outcome, Ok(Vec::new()));
}

#[test]
fn installed_extension_libraries_are_checked() {
    let staged = staged_tree();
    let mut bootstrap = bootstrap_for(&staged);
    bootstrap.installed_extensions = vec![installed_extension_with(&[
        "lib/vector.so",
        "share/extension/vector.control",
    ])];
    let resolver = FakeResolver::missing("vector.so", &["libgomp.so.1"]);

    let err = check_extension_libraries_with(&resolver, &bootstrap).expect_err("fails");

    assert_eq!(err.kind(), BootstrapErrorKind::RuntimeIncompatible);
    assert_eq!(
        *resolver.seen.borrow(),
        [utf8(&staged.path().join("lib/vector.so")).to_string()]
    );
}

#[cfg(feature = "async-api")]
mod async_runtime {
    //! The async check must leave the runtime free while the resolver blocks.

    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::*;
    use crate::runtime_check::stage::check_staged_executables_async_with;

    /// A resolver that blocks its thread, as a slow `ldd` would.
    struct BlockingResolver(Duration);

    impl SharedLibraryResolver for BlockingResolver {
        fn unresolved(&self, _object: &Utf8Path) -> Result<Vec<String>, ResolverUnavailable> {
            std::thread::sleep(self.0);
            Ok(Vec::new())
        }
    }

    /// Counts a tick every 10 ms for as long as the runtime lets it run.
    fn spawn_ticker(ticks: Arc<AtomicUsize>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                ticks.fetch_add(1, Ordering::SeqCst);
            }
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn another_task_progresses_while_the_resolver_blocks() {
        let staged = staged_tree();
        let bootstrap = bootstrap_for(&staged);
        let ticks = Arc::new(AtomicUsize::new(0));
        let ticker = spawn_ticker(Arc::clone(&ticks));

        // Three executables at 100 ms each block a thread for 300 ms. Run
        // inline, that thread is the only one and the ticker never runs.
        check_staged_executables_async_with(
            BlockingResolver(Duration::from_millis(100)),
            &bootstrap,
        )
        .await
        .expect("the check passes");
        ticker.abort();

        assert!(
            ticks.load(Ordering::SeqCst) >= 5,
            "the runtime thread was blocked: {} ticks",
            ticks.load(Ordering::SeqCst)
        );
    }
}
