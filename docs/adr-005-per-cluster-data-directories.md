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
- On Linux, a live process whose `/proc/<pid>/comm` is not `postgres`: the
  PID was reused, so there is no server.
- A live `postgres` process: stopped as `pg_ctl stop -m immediate` stops it,
  with `SIGQUIT` to the postmaster and a wait of up to ten seconds for it to
  exit. The directory is removed only after it has exited.
- A live process that cannot be confirmed (on platforms without a cheap
  name lookup), or a server that does not stop: the slot is left in place, with
  a warning, for a later sweep.

The stop sends the signal directly rather than running `pg_ctl`. This is what
`pg_ctl -m immediate` does, and it needs no binary path from an install tree
that may have changed since the orphan started.

### First setup in a cold root

Processes still share the install tree, so a cold root would otherwise see two
processes copying cached binaries into it, or extracting an archive into it, at
once. The startup lifecycle holds an exclusive lock on
`<install>/.pg-embed-setup.lock` from the binary-cache copy through the server
start. Each process still gets its own server; only their startups take turns.
The asynchronous API takes the lock on the blocking pool, so a second bootstrap
in the same process waits without blocking the runtime.

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
- Startups in one install tree are serialized, so a wide nextest group queues
  briefly at startup. Servers then run in parallel.
- `fs4` is already a dependency (1.1.0, used by the binary cache's lock). The
  only manifest change is the `signal` feature of the existing `nix`
  dependency, used for the liveness probe and the stop.
