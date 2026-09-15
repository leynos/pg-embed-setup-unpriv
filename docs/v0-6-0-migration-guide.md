# v0.6.0 migration guide

This guide covers migration from `v0.5.0` to `v0.6.0`. It is organized by the
change a consumer has to make, breaking changes first.

Each v0.6.0 feature has its own section.

## Bootstrap overrides: `PG_EMBED_ROOT` and `PG_MAX_CONNECTIONS`

Nothing changes for a consumer that sets neither variable. Both are opt-in, and
with both unset every bootstrap resolves exactly as it did in v0.5.x.

### Breaking change: two new `PgEnvCfg` fields

`PgEnvCfg` gains `embed_root: Option<Utf8PathBuf>` and
`max_connections: Option<u32>`. Code that builds the struct with
`..PgEnvCfg::default()` needs no change. A struct literal that lists every
field must add the two new `None`s, or it will not compile.

### `PG_EMBED_ROOT`

Set it to give a project or a test run its own base directory instead of the
per-user tree every project on the host shares.

Table: How the install and data directories are resolved.

| Platform           | `PG_EMBED_ROOT` unset              | `PG_EMBED_ROOT` set |
| ------------------ | ---------------------------------- | ------------------- |
| Linux and the BSDs | `/var/tmp/pg-embed-{uid}`          | the value given     |
| macOS and Windows  | the `postgresql_embedded` defaults | the value given     |

Where a base applies, the leaves are `<root>/install` and `<root>/data`.

Precedence is unchanged and the root sits below the existing variables:
`PG_RUNTIME_DIR` still decides the installation directory and `PG_DATA_DIR`
still decides the data directory, each independently. `PG_EMBED_ROOT` only
supplies the leaf a variable does not name. Setting the root does not move a
directory a consumer has pinned.

The scope is every bootstrap: `TestCluster`, `bootstrap_for_tests()`, `run()`
and the command-line interface, in the unprivileged and the root and worker
paths alike.

Two helpers are exported for consumers that resolve paths themselves:

- `default_paths_under(root)` returns the `install` and `data` leaves for a
  given base, on every platform.
- `default_root_for(uid)` returns the per-user base used when the variable is
  unset. It is exported only on the platforms that have one: Linux, Android,
  FreeBSD, OpenBSD and DragonFly.

`default_paths_for(uid)` is unchanged in signature and in behaviour; it is now
`default_paths_under(default_root_for(uid))`.

### `PG_MAX_CONNECTIONS`

Test clusters cap `max_connections` at 20, well below the 100 a `postgres`
container defaults to. Set this variable when parallel test runners need more.

- **Default.** 20 for a test bootstrap, from the existing worker limits.
  A plain `bootstrap()` run inherits whatever the server defaults to.
- **Scope.** The override applies to plain `bootstrap()` runs as well as test
  bootstraps, and it replaces the test cap rather than being clamped by it.
- **Minimum.** Values below `MIN_MAX_CONNECTIONS`, which is 4 and is exported,
  are rejected when the settings are built, before any directory is touched.
  PostgreSQL keeps three superuser slots below `max_connections`, so a smaller
  value produces a server that refuses to start; failing early with a message
  naming the variable is clearer than that.

`PgEnvCfg::to_settings`, `to_settings_for_tests` and `to_settings_with_context`
therefore gain a new error case. They already returned `Result`, so no
signature changes.

### Observability

Both overrides are visible in the `settings_decision` tracing event, which
carries the resolved installation and data directories, whether each was
derived rather than named, the bounded `root_source` label, and the
`max_connections` the server will actually run at rather than the raw option.

## Password reuse on an existing cluster

Before v0.6.0 every bootstrap generated a fresh superuser password, so a data
directory that already held a cluster (the ordinary state of a shared host, and
of `shared_cluster_handle` across test processes) started a server whose
password nobody knew. From v0.6.0 the bootstrap, in both the unprivileged and
the root/worker paths, reads the password back from the install tree's password
file when the data directory holds a cluster and `PG_PASSWORD` is unset.

What changes for a consumer:

- Nothing, when `PG_PASSWORD` is set: an explicit password always wins.
- Reused clusters become reachable with no configuration change.
- A cluster whose password file is missing, unreadable or empty now fails the
  bootstrap with a message naming the data directory and the file, instead of
  starting an unreachable server. Set `PG_PASSWORD` to the password that
  initialized the cluster, or remove the stale cluster.

New API for consumers that manage `Settings` themselves:

- `stored_cluster_password(data_dir, password_file)` returns
  `Ok(None)` when there is no cluster and the stored password otherwise.
- `reuse_existing_password(settings, data_dir, password_file, explicit)`
  applies it and returns a `PasswordReuseOutcome` (`Reused`, `ExplicitPassword`
  or `NoCluster`), each of which is also an `outcome` label of the
  `password_reuse` tracing event. The event carries four further labels
  (`probe_failed`, `missing_file`, `unreadable_file`, `empty_file`) for the
  failures, which return an error rather than an outcome.
