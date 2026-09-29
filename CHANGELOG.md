# Changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## 0.6.2

### Fixed

- 0.6.1 could not be built on Rust 1.93 or earlier. A direct
  `postgresql_archive` requirement of 0.20.4 pulled in a release that needs
  Rust 1.94. The requirement is now `0.20.2`, the floor `postgresql_embedded`
  names, so a lockfile resolved for an older compiler can pick it.

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
