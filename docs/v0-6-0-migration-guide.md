# v0.6.0 migration guide

## Password reuse on an existing cluster

Before v0.6.0 every bootstrap generated a fresh superuser password, so a
data directory that already held a cluster (the ordinary state of a shared
host, and of `shared_cluster_handle` across test processes) started a server
whose password nobody knew. From v0.6.0 the bootstrap, in both the
unprivileged and the root/worker paths, reads the password back from the
install tree's password file when the data directory holds a cluster and
`PG_PASSWORD` is unset.

What changes for a consumer:

- Nothing, when `PG_PASSWORD` is set: an explicit password always wins.
- Reused clusters become reachable with no configuration change.
- A cluster whose password file is missing, unreadable or empty now fails the
  bootstrap with a message naming the data directory and the file, instead
  of starting an unreachable server. Set `PG_PASSWORD` to the password that
  initialized the cluster, or remove the stale cluster.

New API for consumers that manage `Settings` themselves:

- `stored_cluster_password(data_dir, password_file)` returns
  `Ok(None)` when there is no cluster and the stored password otherwise.
- `reuse_existing_password(settings, data_dir, password_file, explicit)`
  applies it and returns a `PasswordReuseOutcome` (`Reused`,
  `ExplicitPassword` or `NoCluster`), which is also the bounded `outcome`
  label of the `password_reuse` tracing event.
