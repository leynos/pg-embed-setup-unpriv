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
