# v0.8.0 migration guide

This guide covers migration from `v0.7.0` to `v0.8.0`. It is organized by the
change a consumer has to make. One change can break a build: callers that build
`TestBootstrapSettings` with a struct literal must set the new
`runtime_preflight` field. A second change can break a lockfile: the crate now
requires `ortho_config` 0.11. A third alters runtime behaviour: bootstrap now
checks the staged `PostgreSQL` binaries for unresolved shared libraries.

## `TestBootstrapSettings` gains `runtime_preflight`

`TestBootstrapSettings` has a new public field, `runtime_preflight`, of type
`RuntimePreflight`. A caller that builds the struct with a literal must set it,
for example to `RuntimePreflight::Check`.

`RuntimePreflight::Check` is the default and matches the new behaviour. Callers
that obtain settings from `bootstrap_for_tests()` or a `TestBootstrapConfig`
need no change.

## Bootstrap checks for unresolved shared libraries

The bootstrap now asks the host's `ldd` about the staged `postgres`, `initdb`
and `pg_ctl` executables, and the installed extension libraries. A host that
cannot resolve a library fails with `BootstrapErrorKind::RuntimeIncompatible`.
Before this release the same host failed with an opaque start-up error.

- **Typed error.** Downcast the report to `RuntimePreflightError` to read the
  binary, each missing SONAME, the host, the `PostgreSQL` version and the
  archive source. Ubuntu 26.04 lacks `libxml2.so.2`, so it fails this way with
  PostgreSQL 18.x archives.
- **Hosts without `ldd`.** A host without `ldd` fails with
  `BootstrapErrorKind::RuntimeCheckIndeterminate`. Install `libc-bin`, or opt
  out.
- **Opting out.** Call
  `TestBootstrapConfig::with_runtime_preflight(RuntimePreflight::Skip)`. The
  opt-out is a builder option and not an environment variable.
- **Cost.** A passing check leaves a `.pg-embed-runtime-ok` marker beside the
  staged tree, so later runs on the same host read one file. Delete the marker
  to force a new check.
- **Platforms.** Only Linux is checked; macOS and Windows skip the check.

The [users' guide](users-guide.md#supported-host-images-and-runtime-packages)
lists the supported host images and their runtime packages.

## `ortho_config` 0.11

The crate requires `ortho_config` 0.11.0 (it was 0.9.0). A project that pins
`ortho_config` itself must move to 0.11 so that one copy resolves; otherwise
Cargo keeps two copies and the lockfile gains the extra packages that 0.11
brings.

## Nothing else to do

The `ScopedEnv` test changes in this release are internal, and the public API
is otherwise unchanged.
