# pg_embedded_setup_unpriv developer guide

This guide captures contributor-focused notes for maintaining the library. It
complements the user guide and omits consumer-facing usage details.

Consumer integrations should start with `docs/users-guide.md` for
consumer-facing guidance.

## Test coverage notes

- Unit and behavioural tests assert that `postmaster.pid` disappears after
  `TestCluster` teardown, demonstrating that no orphaned processes remain.
- Behavioural tests driven by `rstest-bdd` exercise both privilege branches to
  guard against regressions in ownership or permission handling.
- Behavioural suites coordinate via a shared lock file on Unix and an atomic
  lock directory on non-Unix platforms, so concurrent test binaries do not
  contend over PostgreSQL setup or cache directories. The non-Unix lock keeps a
  short owner grace window for missing, malformed, or unreadable owner files so
  a competing process cannot delete a newly created lock while the owner is
  still being recorded.

## Feature coverage in CI

The default feature set keeps Diesel optional for consumers, while `make test`
enables `--all-features` so the Diesel helpers are exercised by smoke tests. CI
also runs a Linux matrix for unprivileged and root execution. The root variant
invokes the test suite under `sudo` so root-only privilege paths execute, while
the unprivileged variant continues to collect coverage.

macOS and Windows CI legs build the production binaries and run the
unprivileged surface tests. These legs deliberately avoid root privilege-drop
coverage, which is a Linux/Unix allowlisted path. macOS root execution fails
fast through the shared privilege-drop support predicate, while Windows follows
the in-process unprivileged path.

## Release process

Tagging a release with `v*` triggers `.github/workflows/release.yml`. The
workflow creates a draft GitHub release, builds native archives for Linux
`x86_64`/`aarch64`, macOS Apple Silicon/Intel, and Windows x86-64, then uploads
`pg-embed-setup-unpriv-{target}-v{version}.tgz` assets containing both
`pg_embedded_setup_unpriv` and `pg_worker`.

The release workflow invokes `scripts/release_archive.py` through `uv run` so
Python 3.13 and the script dependencies are provisioned explicitly on every
runner. The script builds the selected production binaries, applies the Windows
`.exe` suffix when staging the archive, rejects path-like `target` and
`--binary` values before joining filesystem paths, and writes the shared
`cargo-binstall` `.tgz` layout. `Cargo.toml` exposes matching
`[package.metadata.binstall]` entries so `cargo binstall pg-embed-setup-unpriv`
can install those published assets on the supported host triples.

Pull-request CI also performs a local `cargo-binstall` install-and-run check on
Linux, macOS, and Windows using cargo-binstall 1.19.1. The release workflow
audits published asset URLs with the same pinned cargo-binstall bootstrap
before the draft release is published.

## Windows shutdown hook

Windows shared-cluster cleanup uses a platform-specific shutdown hook rather
than the POSIX signal path. The hook prepares a kill-on-close Job Object for
the validated postmaster process tree and keeps direct `TerminateProcess`
traversal as the forceful fallback. The root PID from `postmaster.pid` is
verified against the live postmaster identity before any action, and
descendants are revalidated against the current root tree before job assignment
or termination so a reused PID is not treated as part of the original cluster.

Debug tracing records Job Object preparation, identity mismatches, descendant
validation skips, assignment attempts, and termination attempts with PID and
outcome fields. These logs are intentionally low-level because shutdown hooks
run during process exit and cannot recover interactively.

The process-tree tests are example-driven rather than property-generated: the
tree collector is a finite closure over the snapshot entries, rejects cycles by
bounding ancestor traversal to the snapshot length, and validates both
termination and Job Object assignment decisions against a reused-descendant-PID
case. The serial lock tests cover missing, partial, malformed, and stale owner
states around the grace window.

## Loom concurrency tests

Loom-based checks for `ScopedEnv` are opt-in and only compile when the
`loom-tests` feature is enabled. The Loom tests are marked `#[ignore]`, and
`make test` keeps them dormant: the nextest run uses `--all-features`, while
the follow-up `cargo test` run disables default features (enabling `dev-worker`
only). Run the Loom suite with:

```sh
cargo test --features "loom-tests" --lib -- --ignored
```

## Further reading

- `tests/e2e_postgresql_embedded_diesel.rs` – example of combining the helper
  with Diesel-based integration tests while running under `root`.
