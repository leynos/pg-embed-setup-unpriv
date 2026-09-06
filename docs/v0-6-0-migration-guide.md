# v0.6.0 migration guide

v0.6.0 adds the prebuilt extension hook. Consumers that do not declare
extensions see no behavioural change: with `PG_EXTENSIONS` unset the hook is
inert and every lifecycle runs exactly as in v0.5.x. The public changes are:

- four new fields on `PgEnvCfg` (a struct literal listing every field must
  add them; `..PgEnvCfg::default()` needs nothing);
- two new fields on `TestBootstrapSettings` (same rule for literals);
- nine new `BootstrapErrorKind` variants, so an exhaustive `match` on that
  enum must gain arms or a wildcard: this is the one change that can fail to
  compile existing code;
- a new public module, `pg_embedded_setup_unpriv::extensions`, and
  `ClusterHandle::installed_extensions()`.

The sections below detail each.

## New configuration

Four environment variables, read through `PgEnvCfg` and therefore honoured by
`TestCluster`, `bootstrap_for_tests`, `run()` and the CLI:

Table: Environment variables added in v0.6.0.

| Variable                        | Purpose                                                                      |
| ------------------------------- | ---------------------------------------------------------------------------- |
| `PG_EXTENSIONS`                 | Comma-separated `CREATE EXTENSION` names to install before the server starts |
| `PG_EXTENSIONS_MANIFEST`        | `https://` URL or path of the `manifest.json` that pins the archives         |
| `PG_EXTENSIONS_MANIFEST_SHA256` | Manifest digest; required for an HTTPS manifest                              |
| `PG_EXTENSIONS_CACHE_DIR`       | Where verified archives are kept between runs                                |

`PgEnvCfg` gains the matching fields `extensions`, `extensions_manifest`,
`extensions_manifest_sha256` and `extensions_cache_dir`. Code that builds a
`PgEnvCfg` with `..PgEnvCfg::default()` needs no change; a literal that lists
every field must add the four new `None`s.

## `TestBootstrapSettings`

Two fields were added: `extensions: Option<ExtensionRequest>` (what was
declared) and `installed_extensions: Vec<InstalledExtension>` (what the hook
installed, filled in once the cluster is up). Code that constructs the struct
literally, which the crate's own fixtures do, must add them; `None` and
`Vec::new()` reproduce the v0.5.x behaviour.

## `BootstrapErrorKind`

Nine variants were added, all prefixed `Extension`. An exhaustive `match` over
the enum must gain arms (or a wildcard); the existing variants and their
meanings are unchanged. See the failure table in
[`docs/extensions.md`](extensions.md).

## New API

- `ClusterHandle::installed_extensions()` reports the installed extensions.
- The `extensions` module exposes `ExtensionRequest::from_config`,
  `install_extensions`, `install_extensions_async`, the validated
  `ExtensionName` and `Sha256Hex` newtypes, `Manifest` and its selection rules,
  and `compile_target()`.

## Behavioural notes

- The hook runs after `Setup` and before `Start` in every lifecycle. Binary
  cache population moved to the same point (before `Start` rather than after
  it) so the shared binary cache never contains extension files; the cache
  layout and keys are unchanged.
- Failures are fail-closed: an unresolvable name, an unmatched PostgreSQL
  major, an unmatched target, a digest mismatch or an invalid archive stops the
  bootstrap before the server starts and nothing is compiled.
- The crate now depends on `reqwest` (blocking, native TLS), `tar` and
  `flate2`.
