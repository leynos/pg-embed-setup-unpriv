# v0.7.0 migration guide

This guide covers migration from `v0.6.x` to `v0.7.0`. It is organized by the
change a consumer has to make. One change can break a build: the minimum
supported Rust version rises to 1.94. The rest need nothing from a consumer
that does nothing, except where noted below.

## Rust 1.94 or newer

The crate's `rust-version` is now 1.94 (it was 1.92), and it depends on
`postgresql_embedded` and `postgresql_archive` 0.22.0, which declare 1.94. A
project on an older toolchain must upgrade before adopting 0.7.0. A project
that keeps 0.6.x stays on 1.92. The `msrv` CI job builds every target at the
declared version.

## Named time zones work on every PostgreSQL release

PostgreSQL archives from 18.6.1 onward store many `share/timezone` files as tar
hard links. `postgresql_archive` before 0.22.0 dropped them, so
`share/timezone/UTC` was missing and any session that named a zone failed with
`invalid value for parameter "TimeZone": "UTC"` (leynos/mxd#665). 0.22.0
extracts hard links, so this release needs no workaround.

- **No default pin.** An unset `PG_VERSION_REQ` accepts any release, as in
  0.6.2 and earlier, so upstream's newest listed release wins. Development
  builds between 0.6.3 and 0.7.0 pinned `=18.6.0` to avoid the failure; that
  pin never shipped in a release. Set `PG_VERSION_REQ` or the configuration
  file's `version_req` to keep an explicit choice. A consumer that carries its
  own `PG_VERSION_REQ` pin or a time-zone alias repair for this failure can
  drop it.
- **Stale cache entries.** An `18.6.1` or later entry that an older release
  extracted still lacks `share/timezone/UTC`, and the cache reuses it. Delete
  it once: see
  [cache troubleshooting](cache.md#cache-entries-and-stale-time-zone-data).

When reusing an installation root with version-named subdirectories, bootstrap
selects the highest installed version accepted by the effective requirement.
Candidates need a `bin/` directory; semver-named versions rejected by the
requirement are ignored. Names that are not semver remain lexical fallbacks
when no eligible semver candidate exists. A root that itself contains `bin/`
continues to be used directly.

`ExtensionName` now implements `FromStr`, so callers can use
`"vector".parse::<ExtensionName>()`. Names must be non-empty and contain only
lower-case ASCII letters, digits or underscores. Invalid input, including
uppercase letters and whitespace, returns
`BootstrapErrorKind::ExtensionConfigInvalid`.

## A killed test process now stops its server (Linux)

From this release, a cluster started on Linux through the library gets a
detached watcher. When the owning process dies by `SIGKILL` or the
out-of-memory killer, the watcher stops the cluster's `postgres` at once,
instead of leaving it until a later bootstrap's sweep (#287).

- **Nothing to do** in the normal case. A cluster stopped normally ends its own
  watcher.
- **Prerequisites.** `setsid` and `flock` from util-linux, and `/proc`. Where
  either tool is missing, and on macOS and Windows, there is no watcher and the
  next bootstrap's sweep is the only reclaim, as before.
- **Opting out.** Set `PG_EMBED_ORPHAN_WATCHER=off` in a process to keep it from
  starting watchers. A test of the sweep needs this: a watcher stops the orphan
  before the sweep has one to find, and holds the slot's lock while it does.
- **Library-only test binaries are covered.** The watcher is a shell script run
  through `setsid flock`, not a helper binary, so nothing has to be installed
  or hooked into `main`.

## Test clusters are non-durable by default

`TestCluster`, `shared_cluster_handle()` and `bootstrap_for_tests()` now run
`initdb --no-sync` and start the server with `fsync`, `synchronous_commit` and
`full_page_writes` off (#297). The setup-only `run` function and the binary are
unchanged and stay durable.

- **Nothing to do** for a suite that treats its cluster as disposable, which is
  nearly every suite. It starts faster, markedly so on a loaded or shared disk.
- **A test that depends on durability must opt out.** Set `PG_EMBED_DURABLE=1`
  for the process that starts the cluster. Examples: a test that kills the
  server and expects committed rows to survive, or one that reads on-disk state
  after an unclean stop. The opt-out states `fsync = on` explicitly, because
  the embedded server is otherwise started with `-F`.
- **A durability setting you put in the configuration wins** over either mode.

## A new metric variant

`observability::Metric` gains `OrphanWatcher(OrphanWatcherOutcomeMetric)`. The
enums are `#[non_exhaustive]`, so a consumer's `match` already needs a wildcard
arm and keeps compiling. A recorder that forwards every metric needs no change;
one that maps variants individually can add the new one. The outcomes are
`Disabled`, `NotASlot`, `SlotUnknown`, `Spawned`, `SpawnFailed`, `Released` and
`ReleaseFailed`; only the library's side is counted, because the watcher
process reports nothing.

## Suites that share a cluster across processes

0.6 stopped persisting the derived data directory across processes. A suite
that relied on 0.5's sharing and runs one process per test now runs `initdb`
per process. Pin `PG_DATA_DIR` (and `PG_RUNTIME_DIR`) before the bootstrap to
restore the sharing; see #306 and the corbusier port for the pattern.
