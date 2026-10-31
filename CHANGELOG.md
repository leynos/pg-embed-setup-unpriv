# Changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## 0.7.0

This release raises the minimum supported Rust version to 1.94.

### Rust 1.94 minimum

- `rust-version` is now 1.94 (it was 1.92), and the crate requires
  `postgresql_embedded` and `postgresql_archive` 0.22.0, which declare 1.94. A
  project on an older toolchain must upgrade before adopting 0.7.0. The
  Dependabot ignore entries for `postgresql_embedded` and `postgresql_archive`
  versions `>= 0.20.3` are removed.

### Slot claim and build script

- A slot claim now holds `.claim-guard` across both the dead-slot sweep and the
  creation and locking of the next slot. Before, the sweep ran before the guard
  was taken, so a concurrent bootstrap could open and lock a newly created but
  not-yet-locked `.lock` file in the gap. The owner then failed with "slot lock
  is already held", and the shared test-cluster singleton cached that failure
  for the process. A regression test pauses the first claim in its sweep and
  after its slot is created and checks that a second claim waits for the guard.
- `build.rs` registers and emits `privileged_unix_platform` directly with
  `cargo:rustc-check-cfg` and `cargo:rustc-cfg`, and the `cfg_aliases` build
  dependency is gone. Its macro expansion triggered a future-incompatible
  trailing-semicolon warning on current toolchains. The cfg is set for Linux,
  Android, FreeBSD, OpenBSD and DragonFly targets, as before.

### Lint toolchain

- The pinned lint toolchain moves from `nightly-2026-04-25` to
  `nightly-2026-10-30` (#357, #371), and the new lints are fixed rather than
  silenced. The 10-30 nightly no longer raises `clippy::cognitive_complexity` on
  `caches_successful_initialization`, so its unfulfilled `#[expect]` is gone.
  `AtomicUsize::fetch_update` is deprecated in favour of `try_update`, but
  `try_update` is only stable from Rust 1.95, above this crate's declared
  minimum, so the privilege-drop counter decrements through a
  `compare_exchange` loop that behaves the same on every supported toolchain. A
  `#[deprecated]` item gains the `since` version the lint now requires, and
  `ExtensionName` gains the `FromStr` implementation Clippy expects alongside
  its existing `TryFrom<&str>`.

### Named time zones on every PostgreSQL release

- The default PostgreSQL version pin to `18.6.0` is dropped. From `18.6.1`
  onward the Linux and macOS archives pack most of `share/timezone` as tar hard
  links, which `postgresql_archive` before 0.22.0 silently dropped, so every
  session that named a zone failed with
  `invalid value for parameter "TimeZone": "UTC"` (#351, leynos/mxd#665).
  `postgresql_archive` 0.22.0 extracts hard links, so an unset `PG_VERSION_REQ`
  accepts any release again. `PG_VERSION_REQ` still selects a version. A cache
  entry that an older release extracted still lacks `share/timezone/UTC`;
  delete it once (see `docs/cache.md`). The named-time-zone regression test now
  runs against an unpinned cluster.
- An installation directory holding several version subdirectories now
  resolves to one the settings' version requirement accepts, instead of always
  taking the highest. Without this an exact pin would be undone by a newer
  directory left behind by an earlier run.

### Non-durable test clusters

- Test clusters are non-durable by default (#297). `TestCluster`,
  `shared_cluster_handle()` and `bootstrap_for_tests()` run `initdb --no-sync`
  and set `fsync`, `synchronous_commit` and `full_page_writes` off, which cuts
  cluster start times sharply on loaded or shared-disk hosts. The setup-only
  `run` function and the binary keep PostgreSQL's durable defaults. Set
  `PG_EMBED_DURABLE=1` to opt out; a test that depends on durability must. The
  first cluster in a cold install tree still runs the synced `initdb`, because
  upstream installs and initializes in one step.

### Orphaned servers

- **A killed test process no longer leaves its server running (Linux).** Before
  a cluster's server starts, the library spawns a small watcher,
  `setsid flock <slot.lock> sh -c …`, that blocks until the kernel releases the
  owner's slot lock and then stops the cluster's `postgres` with `SIGQUIT`, but
  only if `/proc/<pid>/comm` is `postgres` and `/proc/<pid>/cwd` is the data
  directory. It needs no helper binary, so a library-only test binary is
  covered. A cluster stopped normally, or one whose start fails, ends its
  watcher. Where `setsid` or `flock` is missing, as on macOS, the next
  bootstrap's sweep still reclaims the server (#287). Each watcher decision
  (disabled, not a slot, slot unknown, spawned, spawn failed, released, release
  failed) is also recorded as a `Metric::OrphanWatcher` count through the
  installed `MetricsRecorder`.

### Windows scenario lock

- The Windows scenario-lock test helper retries when creating its lock
  directory reports access denied, which Windows returns while another
  process's removal of the directory is still pending, instead of panicking. A
  path that really cannot be created still fails, at the deadline, with the
  last error in the message (#279).

### Zero-configuration load

- `PgEnvCfg::load()` no longer fails when no `PG_*` variable and no `.pg.toml`
  is present. The `ortho_config` 0.9 bump (#302) made an all-empty
  configuration fail with "invalid type: null, expected struct PgEnvCfg", so a
  bootstrap with no configuration at all errored out on `main`; it now yields
  the default configuration and a debug event records the fallback (#317). A
  configuration file that cannot be read still fails the load.

### Windows test targets

- `cargo test` with default features no longer fails to compile `tests/ui.rs`
  on Windows. The target includes fixtures that need the `cluster-unit-tests`
  surface, so it now declares `required-features` like the other test targets.
  CI checks every target with default features on Windows and macOS so a
  feature-dependent target cannot regress unnoticed (#291).

## 0.6.3

### Concurrent starts

- Concurrent cluster starts no longer take turns through `initdb`. A warm root
  serialized every start behind the install tree's setup lock, so start times
  under `cargo nextest` climbed linearly with the number of processes (#289).
  The lock is released after a binary-cache hit, and the cache copy no longer
  fails on a warm tree's symbolic links, which had made every warm start look
  like a cache miss.

## 0.6.2

### Fixed

- 0.6.1 could not be built on Rust 1.93 or earlier. A direct
  `postgresql_archive` requirement of 0.20.4 pulled in a release that needs
  Rust 1.94 (#286). The requirement is now `0.20.2`, the floor
  `postgresql_embedded` names, so a lockfile resolved for an older compiler can
  pick it.

### Minimum supported Rust version

- `rust-version` is now 1.92, the version `postgresql_embedded` 0.20.2 needs.
  The previous 1.85 was never buildable.

### Added

- `make msrv`, run in CI as the `msrv` job, builds every target at the
  declared `rust-version` against a lockfile resolved for it, so a dependency
  that raises the minimum fails CI instead of a downstream build.

## 0.6.1

### Changed

- Each cluster started from a derived root (`PG_EMBED_ROOT` or the per-user
  default) gets its own data directory under `<root>/data`, so concurrent
  processes no longer share one. See ADR 005 and the users' guide.
- A stale password file left by a reaped cluster is discarded, and a failed
  admin connection keeps its underlying error.
- A shared cluster retries a bootstrap that fails for a transient reason,
  a bounded number of times.
