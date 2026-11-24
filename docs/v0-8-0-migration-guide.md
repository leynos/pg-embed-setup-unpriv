# Migration guide: v0.7.0 to v0.8.0

## Who should read this

Read this guide if you depend on `pg-embed-setup-unpriv` 0.7.0 and move to
0.8.0. One change can stop a build: a caller that builds
`TestBootstrapSettings` with a struct literal must set the new
`runtime_preflight` field. A project that pins `ortho_config` itself should
move to 0.11. One change alters runtime behaviour: the bootstrap now checks the
staged `PostgreSQL` binaries for unresolved shared libraries and fails with a
typed error on a host that cannot run them.

## Impact at a glance

| Priority           | Area                                   | What to do                                                          |
| ------------------ | -------------------------------------- | ------------------------------------------------------------------- |
| Required if called | `TestBootstrapSettings` struct literal | Set `runtime_preflight` in every literal.                           |
| Recommended        | `ortho_config` pin                     | Move a project's own `ortho_config` dependency to 0.11.             |
| Review if enabled  | Runtime preflight on Linux             | Check that hosts and images carry the runtime packages, or opt out. |
| Recommended        | Dependency version                     | Raise the requirement to `0.8.0`.                                   |

_Table 1: Obligations when moving from v0.7.0 to v0.8.0._

## 1. Set `runtime_preflight` in struct literals

`TestBootstrapSettings` has a new public field, `runtime_preflight`, of type
`RuntimePreflight`. A caller that obtains settings from `bootstrap_for_tests()`
or a `TestBootstrapConfig` needs no change. A caller that writes the struct out
in full must add the field.

### Before: struct literal

```rust,no_run
use pg_embedded_setup_unpriv::TestBootstrapSettings;

pub fn rebuilt(old: &TestBootstrapSettings) -> TestBootstrapSettings {
    TestBootstrapSettings {
        privileges: old.privileges,
        execution_mode: old.execution_mode,
        settings: old.settings.clone(),
        environment: old.environment.clone(),
        worker_binary: old.worker_binary.clone(),
        setup_timeout: old.setup_timeout,
        start_timeout: old.start_timeout,
        shutdown_timeout: old.shutdown_timeout,
        cleanup_mode: old.cleanup_mode,
        binary_cache_dir: old.binary_cache_dir.clone(),
        extensions: old.extensions.clone(),
        installed_extensions: old.installed_extensions.clone(),
    }
}
```

### After: struct literal

```rust,no_run
use pg_embedded_setup_unpriv::{RuntimePreflight, TestBootstrapSettings};

pub fn rebuilt(old: &TestBootstrapSettings) -> TestBootstrapSettings {
    TestBootstrapSettings {
        privileges: old.privileges,
        execution_mode: old.execution_mode,
        settings: old.settings.clone(),
        environment: old.environment.clone(),
        worker_binary: old.worker_binary.clone(),
        setup_timeout: old.setup_timeout,
        start_timeout: old.start_timeout,
        shutdown_timeout: old.shutdown_timeout,
        cleanup_mode: old.cleanup_mode,
        binary_cache_dir: old.binary_cache_dir.clone(),
        extensions: old.extensions.clone(),
        installed_extensions: old.installed_extensions.clone(),
        runtime_preflight: RuntimePreflight::Check,
    }
}
```

Both examples compile against the release they name (0.7.0 and 0.8.0), and the
first fails to compile against 0.8.0 with the error shown below.

`RuntimePreflight::Check` is the default and matches the behaviour of
`TestBootstrapConfig`. Use `RuntimePreflight::Skip` only to keep the previous
behaviour on purpose.

## 2. Move a pinned `ortho_config` to 0.11

The crate requires `ortho_config` 0.11.0 (it was 0.9.0). A project that pins
`ortho_config` itself should move to 0.11 so that one copy resolves. An older
pin does not always break the build: Cargo can keep two copies, and the
lockfile then gains the extra packages that 0.11 brings.

### Before: `ortho_config` pin

```toml
ortho_config = "0.9.0"
```

### After: `ortho_config` pin

```toml
ortho_config = "0.11.0"
```

## 3. Review hosts for the runtime preflight (Linux)

The bootstrap now asks the host's `ldd` about the staged `postgres`, `initdb`
and `pg_ctl` executables, and the installed extension libraries. A host that
cannot resolve a library fails with `BootstrapErrorKind::RuntimeIncompatible`.
Before this release the same host failed with an opaque start-up error.

- **Typed error.** Downcast the report to `RuntimePreflightError` to read the
  binary, each missing SONAME, the host, the `PostgreSQL` version and the
  archive source. Ubuntu 26.04 lacks `libxml2.so.2`, so it fails this way with
  `PostgreSQL` 18.x archives.
- **Hosts without `ldd`.** A host without `ldd` fails with
  `BootstrapErrorKind::RuntimeCheckIndeterminate`. Install `libc-bin`, or opt
  out. The exception is the explanation of a failed first-download setup: with
  no `ldd` that run returns the original `initdb` failure and its error kind,
  with a note that the preflight was unavailable.
- **Opting out.** Call
  `TestBootstrapConfig::with_runtime_preflight(RuntimePreflight::Skip)`. The
  opt-out is a builder option and not an environment variable.
- **Cost.** A passing check leaves a `.pg-embed-runtime-ok` marker beside the
  staged tree, so later runs on the same host read one file. Delete the marker
  to force a new check.
- **Platforms.** Only Linux is checked. macOS and Windows skip the check.

The [users' guide](users-guide.md#supported-host-images-and-runtime-packages)
lists the supported host images and their runtime packages.

## 4. Raise the dependency requirement

### Before: dependency requirement

```toml
pg-embed-setup-unpriv = "0.7.0"
```

### After: dependency requirement

```toml
pg-embed-setup-unpriv = "0.8.0"
```

## What does not need to change

Callers that use `TestCluster::new()`, `shared_cluster_handle()` or
`bootstrap_for_tests()` on a supported host need no code change. The
`ScopedEnv` test changes in this release are internal, and the rest of the
public API is unchanged.

## Migration checklist

- [ ] Set `runtime_preflight` in every `TestBootstrapSettings` struct literal.
- [ ] Move any project-level `ortho_config` pin to 0.11.0.
- [ ] Confirm that Linux CI images and developer hosts provide `ldd` and the
      runtime packages, or opt out with `RuntimePreflight::Skip`.
- [ ] Raise the `pg-embed-setup-unpriv` requirement to `0.8.0`.

## Common errors and fixes

Compiler error for a struct literal without the new field:

```text
error[E0063]: missing field `runtime_preflight` in initializer of `TestBootstrapSettings`
```

- **Fix:** Add `runtime_preflight: RuntimePreflight::Check` to the literal
  (task 1).

Bootstrap error on a host that lacks a library. On Ubuntu 26.04.1 LTS with the
`PostgreSQL` 18.6.0 archive, `<url>` is
`https://github.com/theseus-rs/postgresql-binaries` and `<os>` is
`Ubuntu 26.04.1 LTS`:

```text
the staged PostgreSQL 18.6.0 (archive source: <url>) cannot run on this host (<os>); unresolved shared libraries:
  /var/tmp/pg-embed-1000/install/18.6.0/bin/postgres: libxml2.so.2
```

- **Fix:** Install the named runtime package or run on a supported host image
  (task 3). When a compatible archive is available, point `with_releases_url`
  at it, and also give the new source its own `binary_cache_dir` and a fresh
  `runtime_dir`; otherwise the staged tree or cached binaries that failed are
  reused. If an existing cluster keeps its explicit `PG_DATA_DIR`, set
  `PG_PASSWORD` to that cluster's password when switching installation
  directories, as the users' guide describes under
  [Pointing `with_releases_url`](users-guide.md#pointing-with_releases_url-at-an-archive-built-for-the-host).

Bootstrap error on a host without a working resolver, which begins:

```text
cannot tell whether the staged PostgreSQL 18.6.0 (archive source: <url>) can run on this host
```

- **Fix:** Install `libc-bin`, or opt out with `RuntimePreflight::Skip` (task
  3).
