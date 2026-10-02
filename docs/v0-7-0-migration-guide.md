# v0.7.0 migration guide

This guide covers migration from `v0.6.x` to the next minor release. It is
organized by the change a consumer has to make. No change in it is breaking: a
consumer that does nothing keeps compiling and behaves as before, except where
noted below.

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
