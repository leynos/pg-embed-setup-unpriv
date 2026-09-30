# ADR 005: per-cluster data directories under a shared root

## Status

Accepted on 2026-09-28, for the release after 0.6.0 (issue #261). A root
derived from `PG_EMBED_ROOT` or the per-user default now holds one data
directory per cluster rather than one per root. An explicit `PG_DATA_DIR` keeps
the previous behaviour.

## Date

2026-09-28.

## Context and problem statement

Under `cargo nextest` every test is its own process. Each process's shared
cluster (`shared_cluster_handle()`) bootstraps a server of its own. Up to 0.6.0
a root derived one data directory, `<root>/data`, and one password file,
`<root>/install/.pgpass`. Two live processes therefore raced `initdb` and
`pg_ctl start` on one directory, and every one but the first failed with
`pg_ctl: another server might be running`. Consumers with a nextest group wider
than one thread had no working configuration.

ADR 003 serializes environment changes within a process and leaves
cross-process coordination out of scope. This ADR adds the cross-process part
that data directories need.

## Decision drivers

- Concurrent processes in one root must each get a working cluster.
- A cluster's files must never be removed from under a process, or a server,
  that is still using them, including after its PID has been reused.
- Crashed processes must not leave state that grows without bound.
- An explicit `PG_DATA_DIR` is a user's instruction and must be honoured
  unchanged.
- No new dependency.

## Considered options

1. **Attach to one shared cluster per root under a lock.** A reference count
   of attached processes would decide when to stop the server. This keeps one
   server per root, but it needs shutdown coordination across processes and
   recovery when a holder crashes, which is a larger and riskier surface.
2. **One data directory per cluster, with a liveness lock.** Chosen.

## Decision outcome

### Layout

A derived data directory becomes the parent of per-cluster "slots":

| Path                        | Holds                                   |
| --------------------------- | --------------------------------------- |
| `<root>/data/<name>/`       | the cluster's data directory            |
| `<root>/data/<name>.pgpass` | its password file, which `initdb` reads |
| `<root>/data/<name>.lock`   | its liveness lock                       |

`<name>` is `<pid>-<nanoseconds>-<counter>`, unique per cluster rather than per
process, because one process can start several clusters. The install tree,
`<root>/install`, stays shared.

The old paths were `<root>/data` for the data directory and
`<root>/install/.pgpass` for the password file. A cluster left at the old data
path is unused, and the bootstrap logs a warning naming it.

### Liveness is a held kernel lock

The claiming process takes an exclusive `fs4` lock on `<name>.lock` and keeps
it until the process exits: `flock(2)` on Unix, `LockFileEx` on Windows. The
kernel releases it however the process ends. A later bootstrap sweeps each slot
whose lock it can take without blocking, and only such slots, because only a
dead owner's lock can be taken. The PID in the name is never consulted, so a
reused PID can neither keep a dead slot nor condemn a live one.

The lock file is opened by `std`, which sets `O_CLOEXEC`, so the `postgres` and
`pg_ctl` children a bootstrap spawns do not inherit the lock. Otherwise a
server that outlived its test process would keep the dead owner's lock held for
ever.

### Orphaned servers

A test process can die while the server it started lives on, holding the data
directory after the lock is gone. Before removing a dead slot, the sweep reads
`<name>/postmaster.pid`:

- No file, an unreadable file, or a PID that is not running: no server.
- A live process whose name is not `postgres`: the PID was reused, so there is
  no server. The name comes from `/proc/<pid>/comm` on Linux and from
  `ps -o comm=` on other Unix platforms.
- A live `postgres` process that is not serving this slot's data directory:
  the PID was reused by an unrelated server, so there is no server here and
  nothing is signalled. On Linux the test is the process's working directory,
  which a postmaster sets to its data directory and `/proc/<pid>/cwd` reports
  whatever the process title says. A working directory that cannot be read, as
  for another user's process, leaves the slot unconfirmed. Elsewhere the name
  comes from `ps` and the working directory from `lsof -d cwd`, which no
  process can rewrite as it can its title; a missing or failing `lsof`, or an
  unreadable directory, leaves the slot unconfirmed, not absent.
- A live `postgres` process serving this directory: stopped as
  `pg_ctl stop -m immediate` stops it, with `SIGQUIT` to the postmaster and a
  wait of up to ten seconds for it to exit. The directory is removed only after
  it has exited.
- A live process that cannot be confirmed (on Windows, which has no name
  lookup here, or where `ps` or `lsof` fails), or a server that does not stop:
  the slot is left in place, with a warning, for a later sweep.

The stop sends the signal directly rather than running `pg_ctl`. This is what
`pg_ctl -m immediate` does, and it needs no binary path from an install tree
that may have changed since the orphan started.

### First setup in a cold root

Processes still share the install tree, so a cold root would otherwise see two
processes copying cached binaries into it, or extracting an archive into it, at
once. The startup lifecycle holds an exclusive lock on
`<install>/.pg-embed-setup.lock` while the tree is populated: from the
binary-cache copy, and through `Setup` on a cache miss, where `Setup` downloads
and extracts. After a cache hit the tree is complete, and `Setup` only runs
`initdb` in the cluster's own data directory, so the lock is released before
it. The extension hook writes into the tree, so it retakes the lock when
extensions are declared. Each process gets its own server, and on a warm root
only the brief cache copy is serialized. The asynchronous API takes the lock on
the blocking pool, so a second bootstrap in the same process waits without
blocking the runtime.

Holding the lock through `initdb` as well was the first design, and it
serialized every warm start: sixteen concurrent starts finished at 0.8 s to
12.9 s instead of together (#289). Measured with `tests/start_ramp.rs` on a
RAM-backed root, so disk contention does not hide the step:

| Concurrent starts | Lock held through `initdb` (wall, median/max) | Released after the cache hit |
| ----------------- | --------------------------------------------- | ---------------------------- |
| 1                 | 0.83 s / 0.83 s                               | 0.66 s / 0.66 s              |
| 4                 | 3.01 s / 3.74 s                               | 0.82 s / 0.84 s              |
| 8                 | 3.76 s / 6.81 s                               | 0.79 s / 0.84 s              |
| 16                | 7.62 s / 12.95 s                              | 1.12 s / 1.19 s              |

_Table: Wall time to start one cluster per process against a warm root._

A second defect hid this: the cache copy failed with "File exists" on every
symbolic link of a warm tree, so a warm start reported a cache miss and kept
the lock through `Setup` whatever the design. The copy now leaves a link that
already points at its target and replaces any other.

The install tree also stays in use after a startup: the first process's server
runs from it while later processes start. The binary-cache copy therefore
leaves a file alone when the target already holds one of the cached size.
Rewriting a running binary fails with `ETXTBSY` on Linux and, on macOS,
invalidates the code signature so the kernel kills the server ("Killed: 9",
seen on the macOS CI leg). A copy cut short has the wrong size and is copied
again.

### Setup-only bootstraps

`run`, and the binary that calls it, initialize a cluster for use after they
exit. A slot would be swept once its process is gone, so a bootstrap whose kind
is setup-only keeps the derived `<root>/data` directory itself, as 0.6.0 did
(`DataLayout::Persistent`). A `TestCluster` in the same root claims a slot and
does not reuse that directory; `PG_DATA_DIR` shares one. Two setup-only runs in
one root still share `<root>/data`, as before.

### Full cleanup

`CleanupMode::Full` removes the install tree. With clusters running side by
side from it, that would pull the binaries and extensions from under the
others, so a full cleanup demotes itself to removing the data directory when
another slot in the root holds its lock. The rule applies before the in-process
and worker paths choose what to delete, so they agree. If a peer's lock cannot
be probed, the tree is kept.

A cluster could still claim its slot between the probe and the removal. A guard
file, `<root>/data/.claim-guard`, closes that: every slot claim takes an
exclusive lock on it, and a full cleanup holds it across the probe and the
removal. A cluster then either claimed its slot before the probe, and is seen
as a peer, or claims it after the removal, and provisions the tree afresh. A
guard that cannot be taken keeps the tree. An explicit `PG_DATA_DIR` is not a
slot, but its install tree can still be the derived `<root>/install`, so a full
cleanup of it takes the same guard on `<root>/data` and keeps the tree while a
slot there holds its lock. If `<root>/data` does not exist yet, the cleanup
creates it first, as a first claim would, so the two contend on one guard file.
The file does not end in `.lock`, so the sweep never reads it as a slot.

### Explicit `PG_DATA_DIR`

An explicit `PG_DATA_DIR` claims no slot and triggers no sweep. Its password
file stays at `<install>/.pgpass`, as before, so one install tree then serves
one such cluster at a time.

## Consequences

- nextest consumers can run their PostgreSQL tests at any thread count in one
  root.
- A slot's lock file and password file outlive a cluster that is dropped
  before its process exits, until that process exits and a later bootstrap
  sweeps them.
- Populating an install tree is serialized, so a cold root sets up once. A warm
  root serializes only the cache copy, and `initdb` and the server start run in
  parallel.
- `fs4` is already a dependency (1.1.0, used by the binary cache's lock). The
  only manifest change is the `signal` feature of the existing `nix`
  dependency, used for the liveness probe and the stop.
