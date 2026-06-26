# Validate Windows and macOS support via a CI matrix and cargo binstall

This ExecPlan (execution plan) is a living document. The sections `Constraints`,
`Tolerances`, `Risks`, `Progress`, `Surprises & discoveries`, `Decision log`,
and `Outcomes & retrospective` must be kept up to date as work proceeds. Each
revision must remain self-contained.

Status: IN PROGRESS

No `PLANS.md` file exists in the repository.

## Purpose / big picture

Today `pg-embed-setup-unpriv` is built, tested, and packaged for Linux only.
Continuous Integration (CI) runs solely on `ubuntu-latest`, and the release
workflow publishes `cargo binstall` archives for `x86_64-unknown-linux-gnu` and
`aarch64-unknown-linux-gnu`. The library already contains substantial
platform-conditional code — the privilege-drop path and the `atexit` shutdown
hook are already gated off non-Unix targets — and the roadmap appendix already
states the intended behaviour on macOS and Windows. But nothing in CI proves
that the crate even compiles on those platforms, let alone that its
unprivileged in-process path works, that shared clusters are cleaned up
correctly, or that `cargo binstall` can install the published binaries there.

After this change a contributor can observe the following:

1. A pull request runs CI jobs on macOS and Windows runners (in addition to the
   existing Linux jobs). The macOS and Windows jobs build the crate and run the
   unprivileged test suite to green. They run tests only: they do not run the
   Linux-only root/privilege-drop path, coverage upload, or the privileged
   worker tests.
2. Shared `TestCluster` fixtures do not leak a PostgreSQL postmaster on macOS or
   Windows, even when a test deliberately leaks the guard with
   `std::mem::forget` (the pattern the shutdown hook exists to serve). This is
   proven by an orphan-detection test that runs in the cross-platform matrix.
3. `cargo binstall pg-embed-setup-unpriv` succeeds on macOS (Apple Silicon and
   Intel) and on Windows (x86-64), pulling a correctly named archive whose
   internal layout matches the `bin-dir` template. This is proven by a CI job
   that performs a real (not dry-run) install into a scratch directory and runs
   the installed binary's `--version` on every supported runner, plus a
   per-target metadata-resolution audit.
4. The README install matrix, the users' guide, and the roadmap appendix
   describe the supported platforms and their caveats accurately, including the
   hard limitation that no Windows-on-ARM PostgreSQL binaries exist upstream
   and that POSIX file-mode privacy guarantees do not hold on Windows.

The user-visible outcome is simple to state: the same `TestCluster` test code
that passes on Linux also passes on macOS and Windows unprivileged, leaves no
orphaned processes, and the CLI binaries install via `cargo binstall` on all
three operating systems.

## Constraints

These are hard invariants. Violating one requires escalation, not a workaround.

- Preserve all existing public APIs, the command-line interface (CLI), and the
  Linux privilege-drop behaviour. The macOS and Windows work must be additive;
  the privilege-drop path stays Linux/BSD-only and must keep compiling and
  passing on Linux exactly as before.
- The macOS and Windows CI legs run tests only. They must not run `make lint`,
  `make check-fmt`, Markdown lint, coverage generation, or the CodeScene
  coverage upload (those remain Linux-only, single-platform jobs). Linting once
  on Linux is authoritative; duplicating it per-OS is out of scope.
  `upload-codescene-coverage` has no Windows path and must never run there.
- Do not weaken the Clippy gate (`-D warnings`), the lint configuration in
  `Cargo.toml` and `clippy.toml`, or the `RUSTFLAGS="-D warnings"` test
  invocation. New platform-conditional code must satisfy the same lint ceiling.
- Use caret version requirements for any new dependency, per `AGENTS.md`. Do not
  add wildcard or open-ended requirements.
- Prefer reusing the `leynos/shared-actions` composite actions over hand-rolled
  workflow logic, since the user nominated that repository as the reference for
  this work. In particular, prefer `stage-release-artefacts` (which already
  produces a `cargo binstall` archive with Windows path handling and a
  `.sha256` sidecar) and the `rust-toy-app.yml` cross-OS matrix shape over
  bespoke Makefile/zip branching, unless a documented reason makes the action
  unsuitable.
- Any helper script that is genuinely required (after the reuse-first
  evaluation above) must follow the df12 scripting standards: a single-file
  Python program targeting Python 3.13 with a `uv` shebang and inline metadata
  block, using `cyclopts` for the CLI, `cuprum` for external process
  invocation, and `pathlib` for all filesystem work so it runs unchanged on
  POSIX and Windows. Its tests live in `scripts/tests/` mirroring the script
  name and use `pytest` with `cmd-mox`. Prefer not to add such a script if a
  shared action or a small Rust integration test covers the need.
- Pin every third-party GitHub Action by commit SHA, matching the existing
  workflow style. `leynos/shared-actions` references must use a single,
  consistent pin.
- Documentation must use en-GB-oxendict spelling, wrap prose and bullets at 80
  columns, and wrap fenced code at 120 columns. Update the `docs/` knowledge
  base (roadmap, users' guide, design doc) when behaviour changes, per
  `AGENTS.md`.
- All commit gateways (`make check-fmt`, `make lint`, `make test`,
  `make markdownlint`, `make nixie`) must pass on Linux before each commit, as
  enforced today.

## Tolerances (exception triggers)

Adjust as the work proceeds; breaching any of these means stop and escalate
rather than improvise. Note the scope figures below are provisional: they are
re-baselined by Milestone 0's actual cross-compile output, because the precise
blocker set must be observed, not predicted.

- Scope of library portability (Milestone 1): if making the crate compile and
  cleanly cleanup on Windows and macOS requires touching more than 15 source
  files or more than 500 net lines of non-test code, stop and escalate. (Raised
  from an earlier estimate after review found the blocker set differs from the
  first survey; confirm against Milestone 0.)
- Interface: if any public API signature, trait bound, or exported item must
  change (beyond adding platform-conditional `cfg` attributes to existing
  items, or removing a confirmed-dead dependency), stop and escalate.
- Dependencies: if a new runtime dependency beyond reorganising or removing the
  existing `nix`/`xdg`/`openssl-sys` declarations is required, or if a build
  tool (for example Perl or NASM on the Windows runner) cannot be provisioned
  by the existing `setup-rust` action, stop and escalate.
- Cleanup correctness: if the Windows orphan-detection test cannot be made to
  pass after two documented attempts (whether by a real reaper or by proving no
  Windows path leaks a guard), stop and escalate; do not ship a green
  cross-platform badge over a leaking postmaster.
- PostgreSQL feasibility: if the embedded PostgreSQL backend cannot actually
  start on a Windows or macOS runner after two distinct, documented mitigation
  attempts (for example TCP-vs-socket handling, cache path, or `GITHUB_TOKEN`
  rate-limit fixes), stop and escalate with the failing logs; do not silently
  mark those tests as skipped without recording the decision.
- binstall validation: if a real install-and-run cannot be validated in CI
  after one documented approach plus one alternative, stop and present the
  options.
- Iterations: if any single CI leg still fails after three targeted fixes, stop
  and escalate with the run URL and logs.
- Time: if any single milestone exceeds four hours of active work, stop and
  record progress before continuing.
- Cost: if the added CI minutes per pull request exceed the budget recorded in
  Milestone 2 by more than 50%, stop and reconsider caching/matrix scope.
- Ambiguity: if a materially different interpretation emerges (for example
  whether Windows-on-ARM must be a `binstall` target despite having no
  PostgreSQL binaries, or whether macOS/Windows `binstall` binaries are
  required at all versus library-only support), stop and present options with
  trade-offs.

## Risks

The first survey for this plan mis-read the tree and named `shutdown_hook` as
the primary compile blocker; it is in fact already gated. The risk register
below has since been re-grounded by Milestone 0 and updated as mitigations were
implemented.

- Risk (mitigated): `src/fs.rs` originally compiled Unix-only
  `cap_std::fs::PermissionsExt`/`Permissions::from_mode(mode)` calls on every
  target. Severity: high. Mitigation implemented: POSIX mode application is now
  `#[cfg(unix)]`; Windows creates directories without POSIX file-mode privacy
  guarantees, which must be documented for users.
- Risk (mitigated): `nix` was declared as an unconditional dependency and does
  not build on Windows. Severity: high. Mitigation implemented: `nix` is now
  under `[target.'cfg(unix)'.dependencies]`; macOS keeps it as a Unix target.
- Risk (mitigated): `tests/settings.rs` originally imported `geteuid`
  unconditionally, so `--all-targets` failed once `nix` became Unix-only.
  Severity: high. Mitigation implemented: root-specific imports and assertions
  are gated to root-capable Unix targets.
- Risk (mitigated): `xdg = "3"` was a dead direct dependency and could block
  Windows builds. Severity: medium. Mitigation implemented: remove the dead
  dependency; the remaining cache path identifiers are local field/function
  names, not crate uses.
- Risk (mitigated): shared `TestCluster` fixtures that leak the guard with
  `std::mem::forget` rely on process-exit cleanup. Severity: high. Mitigation
  implemented: macOS uses the existing POSIX shutdown hook; Windows now assigns
  the PostgreSQL process tree to a kill-on-close Job Object and retains direct
  process-tree termination as a fallback, guarded by PID plus start-time
  identity checks. The orphan-detection test now runs in the cross-platform CI
  matrix.
- Risk: `openssl-sys` was an unconditional dependency with the `vendored`
  feature (`Cargo.toml` line 140 before Milestone 1). Building vendored OpenSSL
  on Windows MSVC requires Perl and NASM, and cross-checking macOS from Linux
  sends Darwin compiler flags to the Linux host compiler. Severity: medium.
  Likelihood: observed. Mitigation: Milestone 1 removes the crate's direct
  vendored `openssl-sys` dependency. The remaining `postgresql_embedded` default
  `native-tls` graph uses platform TLS on Windows and macOS, so `openssl-sys`
  no longer appears in those target checks; Linux continues to use the native
  OpenSSL path validated by the existing Linux gates.
- Risk: `pq-sys` (bundled, behind `diesel-support`) builds libpq from source and
  may fail or be slow on Windows. Severity: medium. Likelihood: medium.
  Mitigation: scope the Windows test job to exclude `diesel-support`; enable it
  later as a deliberate, separately budgeted follow-up; record the outcome.
  Apply the same feature discipline to macOS rather than silently using
  `--all-features` there (which would pull `pq-sys` at ~10x runner billing).
- Risk: the embedded PostgreSQL backend downloads binaries at runtime from the
  theseus release host and may hit `api.github.com` rate limits (the token
  raises but does not partition the limit, so concurrent matrix legs across PRs
  share the budget), or fail on Windows because of Unix-socket assumptions (the
  embedded settings ignore Unix sockets on Windows and use TCP). Severity:
  medium. Likelihood: medium-high. Mitigation: make the theseus download a
  mandatory `actions/cache` keyed on the pinned PostgreSQL version + OS + arch
  (not "where practical"); pass `GITHUB_TOKEN`; add bounded retries around
  cluster start; decide now whether the new legs are required or advisory and
  record it; verify TCP connection on Windows. A red leg must be
  distinguishable between "crate broken" and "download throttled".
- Risk: there are no `aarch64-pc-windows-msvc` PostgreSQL binaries upstream, so
  the embedded cluster cannot run on Windows-on-ARM. Severity: low (scope
  only). Likelihood: high (certain). Mitigation: do not target Windows-on-ARM
  for tests or `binstall`; document the limitation.
- Risk: `--dry-run` resolves and fetches a `binstall` asset but does not extract
  or place the binary, so it cannot catch a wrong *internal* archive path. A
  published archive with the wrong directory or binary name would pass a
  dry-run yet install nothing. Severity: high. Likelihood: medium. Mitigation:
  validate with a real `cargo binstall --install-path <tmp>` (no `--dry-run`)
  followed by executing the installed binary, on each OS.
- Risk: release packaging previously lived in `make release-archive`, which
  assumed POSIX-style extensionless binaries. The Windows runner does not list
  GNU Make as a supported tool, and Windows release binaries require `.exe`
  names inside the archive. Severity: high. Likelihood: medium. Mitigation: use
  one `uv` script as the release archive implementation and let
  `make release-archive` delegate to it for local development; the release and
  pull-request workflows call the same script directly.
- Risk: `macos-latest` is now Apple Silicon (`aarch64-apple-darwin`), so an
  unqualified macOS job no longer produces `x86_64-apple-darwin`, and the Intel
  archive would ship without ever being executed in CI (dry-run resolves but
  does not run it). Severity: low-medium. Likelihood: high. Mitigation: either
  add an Intel-macOS run leg, or document that Intel macOS is build-and-resolve
  validated only, and accept that gap deliberately.

## Progress

- [x] (2026-06-25T13:18:31Z) Milestone 0 (prototype, gating): run a *real*
  `cargo check`/build for `x86_64-pc-windows-msvc`, `x86_64-apple-darwin`, and
  `aarch64-apple-darwin` against current HEAD; enumerate every actual compile
  error and re-ground the Risk register from observed output (not prediction).
- [x] (2026-06-25T13:31:00Z) Milestone 1 dependency portability: remove the
  direct vendored `openssl-sys` dependency, keep `postgresql_embedded` on its
  default native-TLS backend, move `nix` behind `cfg(unix)`, remove the dead
  direct `xdg` dependency, and re-run the three cross-target checks to reveal
  any remaining source blockers. Evidence:
  `/tmp/check-windows-after-lock-test-gate-windows-mac-support-validation.out`,
  `/tmp/check-darwin-after-test-gates-windows-mac-support-validation.out`, and
  `/tmp/check-darwin-arm-after-test-gates-windows-mac-support-validation.out`.
- [x] (2026-06-25T13:42:00Z) Milestone 1 source portability: cfg-split
  POSIX-only filesystem mode application, Unix-only privilege-drop preparation,
  worker discovery, worker subprocess invocation, and Unix-specific test
  helpers so the library, `pg_embedded_setup_unpriv`, and `pg_worker` typecheck
  on `x86_64-pc-windows-msvc`, `x86_64-apple-darwin`, and
  `aarch64-apple-darwin`. Re-ran all three target checks with
  `RUSTFLAGS="-D warnings"`; evidence:
  `/tmp/check-windows-deny-warnings-windows-mac-support-validation.out`,
  `/tmp/check-darwin-deny-warnings-windows-mac-support-validation.out`, and
  `/tmp/check-darwin-arm-deny-warnings-windows-mac-support-validation.out`.
- [x] (2026-06-25T13:46:00Z) First Milestone 1 portability commit gates passed
  on Linux: `make check-fmt`, `make lint`, `make test`, `make markdownlint`, and
  `make nixie`. Evidence: `/tmp/fmt-windows-mac-support-validation.out`,
  `/tmp/lint-windows-mac-support-validation.out`,
  `/tmp/test-windows-mac-support-validation.out`,
  `/tmp/mdlint-windows-mac-support-validation.out`, and
  `/tmp/nixie-windows-mac-support-validation.out`. The test gate ran two
  nextest passes: `270` passed with `4` skipped for all targets/all features,
  then `151` passed with `0` skipped for the `dev-worker` feature pass.
- [x] (2026-06-25T13:52:00Z) CodeRabbit reviewed commit `dd06cf7` after the
  deterministic gates passed. `coderabbit review --agent` completed with
  `status=review_completed` and `findings=0`, so no portability concerns needed
  clearing before the Windows cleanup milestone.
- [x] (2026-06-25T14:05:00Z) Implemented Windows shared-cluster cleanup by
  compiling the process-exit shutdown hook on Windows, using `taskkill /T` for
  graceful and forced process-tree termination, and adding a private Win32
  `OpenProcess`/`GetExitCodeProcess` liveness probe. The public
  `register_shutdown_on_exit` signature remains unchanged; the test-only PID
  helper is now a platform alias. Focused checks pass for
  `x86_64-pc-windows-msvc`, `x86_64-apple-darwin`, and `aarch64-apple-darwin`,
  including `RUSTFLAGS="-D warnings"` variants, with evidence in
  `/tmp/check-windows-shutdown-hook-windows-mac-support-validation.out`,
  `/tmp/check-windows-deny-warnings-shutdown-hook-windows-mac-support-validation.out`,
  `/tmp/check-darwin-shutdown-hook-windows-mac-support-validation.out`,
  `/tmp/check-darwin-deny-warnings-shutdown-hook-windows-mac-support-validation.out`,
  `/tmp/check-darwin-arm-shutdown-hook-windows-mac-support-validation.out`, and
  `/tmp/check-darwin-arm-deny-warnings-shutdown-hook-windows-mac-support-validation.out`.
- [x] (2026-06-25T14:06:00Z) The orphan-detection lifecycle test now runs by
  default on Unix and Windows targets. Local Linux execution passed with the
  child process leaking the guard, exiting through the atexit hook, and the
  parent observing the postmaster exit. Evidence:
  `/tmp/test-shutdown-hook-lifecycle-windows-mac-support-validation.out`.
- [x] (2026-06-25T14:10:00Z) Windows cleanup milestone gates passed after the
  shutdown-hook tests were serialized with the existing cross-process scenario
  guard. Evidence: `/tmp/fmt-shutdown-hook-windows-mac-support-validation.out`,
  `/tmp/lint-shutdown-hook-windows-mac-support-validation.out`,
  `/tmp/test-shutdown-hook-windows-mac-support-validation.out`,
  `/tmp/mdlint-shutdown-hook-windows-mac-support-validation.out`, and
  `/tmp/nixie-shutdown-hook-windows-mac-support-validation.out`. The test gate
  ran two nextest passes: `275` passed with `3` skipped for all targets/all
  features, then `151` passed with `0` skipped for the `dev-worker` feature
  pass. Final cross-target checks also pass for `x86_64-pc-windows-msvc`,
  `x86_64-apple-darwin`, and `aarch64-apple-darwin`, including
  `RUSTFLAGS="-D warnings"` variants, with evidence in the same
  `/tmp/check-*-shutdown-hook-windows-mac-support-validation.out` logs listed
  above.
- [x] (2026-06-25T14:15:00Z) Strengthened the shared test serial guard for the
  upcoming Windows CI leg: non-Unix platforms now use an atomic lock directory
  instead of a no-op process lock, so independent nextest binaries coordinate
  access to the shared `PostgreSQL` data/cache directories. Re-ran
  `x86_64-pc-windows-msvc`, `x86_64-apple-darwin`, and `aarch64-apple-darwin`
  with `RUSTFLAGS="-D warnings"` plus the full Linux gates; evidence remains in
  the shutdown-hook `/tmp/*` gate logs.
- [x] (2026-06-25T14:25:00Z) CodeRabbit reviewed commit `fd668fe` after the
  deterministic gates passed. `coderabbit review --agent` completed with
  `status=review_completed` and `findings=0`, so no Windows cleanup concerns
  needed clearing before the CI matrix milestone.
- [x] (2026-06-25T14:31:45Z) Implemented the Milestone 2 workflow shape
  locally: added workflow-level cancellation, bumped all
  `leynos/shared-actions` references to verified HEAD
  `7da7c6d89033d13cbb1c64803d108ddca97e69c2`, added a required `macos-latest`/
  `windows-latest` test-only job, pinned `actions/cache` v4.3.0 to
  `0057852bfaa89a56745cba8c7296529d2fc39830`, and configured the cross-platform
  test legs to build both regular binaries, run the CLI `--version` smoke test,
  restore/cache `PG_BINARY_CACHE_DIR`, and run the `cluster-unit-tests` plus
  `async-api` nextest feature set. Local command validation passed for the same
  feature set with `245` tests passed and `1` skipped; evidence:
  `/tmp/build-bins-cross-feature-windows-mac-support-validation.out`,
  `/tmp/test-cross-feature-windows-mac-support-validation.out`, and
  `/tmp/actionlint-ci-matrix-windows-mac-support-validation.out`.
- [x] (2026-06-25T14:43:00Z) CI matrix milestone deterministic gates passed
  locally before CodeRabbit review. Evidence:
  `/tmp/actionlint-ci-matrix-final-windows-mac-support-validation.out`,
  `/tmp/fmt-ci-matrix-windows-mac-support-validation.out`,
  `/tmp/lint-ci-matrix-windows-mac-support-validation.out`,
  `/tmp/test-ci-matrix-windows-mac-support-validation.out`,
  `/tmp/mdlint-ci-matrix-windows-mac-support-validation.out`, and
  `/tmp/nixie-ci-matrix-windows-mac-support-validation.out`. The full test gate
  ran two nextest passes: `275` passed with `3` skipped, then `151` passed with
  `0` skipped.
- [x] (2026-06-25T14:53:00Z) CodeRabbit reviewed commit `341d330` after the
  deterministic gates passed. `coderabbit review --agent` completed with
  `status=review_completed` and `findings=0`, so the CI matrix milestone had no
  concerns to clear before remote CI observation.
- [x] (2026-06-25T15:03:00Z) Remote CI run `28178598283` exposed a macOS
  compile failure in the new `Test (aarch64-apple-darwin)` leg: the
  `cluster-unit-tests,async-api` feature set compiled Linux/BSD root bootstrap
  tests that import `nobody_uid`, but that helper is intentionally not exported
  on macOS. The fix gates `bootstrap_privileges` and the root-specific settings
  tests to the same root-capable Unix target set as the public helper exports,
  and gates a Unix-only `rstest` import in `tests/support/serial.rs` after the
  Windows feature check found it as an unused import. Local evidence:
  `/tmp/check-darwin-ci-feature-root-target-gates-windows-mac-support-validation.out`
  and
  `/tmp/check-windows-ci-feature-root-target-gates-windows-mac-support-validation.out`.
- [x] (2026-06-25T15:14:00Z) CodeRabbit reviewed corrective commit `15ff4a1`
  after the cross-target checks and full local gates passed.
  `coderabbit review --agent` completed with `status=review_completed` and
  `findings=0`.
- [x] (2026-06-25T15:31:00Z) Remote CI run `28179624023` exposed two
  follow-up items after corrective commit `15ff4a1`: the macOS leg reached
  runtime and failed only because
  `discover_worker_errors_on_non_utf8_path_entry` tries to create a
  deliberately invalid byte sequence that APFS rejects with "Illegal byte
  sequence" before the bootstrap helper is exercised; the Windows leg failed
  earlier while crates.io reset the connection downloading `cap-primitives`, so
  it should be retried after the workflow completes rather than treated as a
  source failure. The macOS fix gates that single fixture-style test away from
  macOS while keeping the PATH-absent worker-discovery test active there. Local
  evidence:
  `/tmp/check-darwin-non-utf8-env-test-windows-mac-support-validation.out`,
  `/tmp/check-windows-non-utf8-env-test-windows-mac-support-validation.out`,
  `/tmp/test-ci-feature-non-utf8-env-test-windows-mac-support-validation.out`,
  `/tmp/check-fmt-non-utf8-env-test-windows-mac-support-validation.out`,
  `/tmp/lint-non-utf8-env-test-windows-mac-support-validation.out`,
  `/tmp/test-non-utf8-env-test-windows-mac-support-validation.out`,
  `/tmp/mdlint-non-utf8-env-test-windows-mac-support-validation.out`, and
  `/tmp/nixie-non-utf8-env-test-windows-mac-support-validation.out`.
- [x] (2026-06-25T17:20:00Z) CodeRabbit reviewed corrective commit `52288e4`
  after the deterministic gates passed. The first CLI review attempt hit the
  free allowance rate limit, so the mandated `vsleep $(shuf -i 45-90 -n 1)m`
  backoff was run before retrying. The retry completed with
  `status=review_completed` and `findings=0`.
- [x] (2026-06-25T16:53:37Z) Remote CI run `28185956153` exposed two
  deterministic test-portability assumptions after corrective commit `52288e4`:
  the macOS test leg built both binaries and passed the CLI smoke test, then
  failed only because `staged_worker_is_world_executable_and_in_temp_dir`
  assumes the system temp directory is world-executable for the `nobody` user;
  the Windows test leg built both binaries and passed the CLI smoke test, then
  failed because `resolve_cache_dir_respects_env_priority` compared hard-coded
  POSIX-style strings instead of platform path values. Linux root and
  unprivileged jobs were green in the same run. The fix gates the root/nobody
  temp traversal assertion to root-capable Unix targets and makes the cache
  test build expected values through `PathBuf`/`Utf8PathBuf`.
- [x] (2026-06-25T16:56:43Z) Local validation for the run `28185956153`
  corrective patch passed before commit. Cross-target checks passed for
  `aarch64-apple-darwin` and `x86_64-pc-windows-msvc` with
  `RUSTFLAGS="-D warnings"` and the CI feature set. Focused tests passed:
  `resolve_cache_dir_respects_env_priority` ran `4` cases, and
  `test_support::worker_env::tests` ran `6` cases. Full commit gates passed:
  `make check-fmt`, `make lint`, `make test`, `make markdownlint`, and
  `make nixie`; the test gate ran `275` tests with `3` skipped and then `151`
  `dev-worker` tests with `0` skipped. Evidence:
  `/tmp/check-darwin-path-worker-env-ci-fix-windows-mac-support-validation.out`,
  `/tmp/check-windows-path-worker-env-ci-fix-windows-mac-support-validation.out`,
  `/tmp/test-cache-config-paths-windows-mac-support-validation.out`,
  `/tmp/test-worker-env-path-gate-windows-mac-support-validation.out`,
  `/tmp/check-fmt-path-worker-env-ci-fix-windows-mac-support-validation.out`,
  `/tmp/lint-path-worker-env-ci-fix-windows-mac-support-validation.out`,
  `/tmp/test-path-worker-env-ci-fix-windows-mac-support-validation.out`,
  `/tmp/mdlint-path-worker-env-ci-fix-windows-mac-support-validation.out`, and
  `/tmp/nixie-path-worker-env-ci-fix-windows-mac-support-validation.out`.
- [x] (2026-06-25T18:40:26Z) CodeRabbit reviewed corrective commit `4047e62`
  after deterministic gates passed. Two initial CLI attempts stalled after
  summarization and were stopped by exact process group; the next attempt
  returned the free CLI rate limit, so the mandated `vsleep` backoff ran for
  `89` minutes before retrying. The post-backoff retry completed with
  `status=review_completed` and `findings=0`.
- [x] (2026-06-25T18:55:30Z) Remote CI run `28192593707` for commit
  `df8afb5` showed Linux root, Linux unprivileged, and macOS all green. Windows
  built both binaries and passed the CLI smoke test, then failed only in
  `shutdown_hook_lifecycle::postmaster_exits_after_child_process_with_shutdown_hook`:
  the parent observed postmaster PID `2388` still running after the child
  exited and waited `30s`. The runner cleanup also found orphaned `postgres`
  processes (`2388` and `8056`). The fix replaces the Windows shutdown hook's
  `taskkill` shell-out with direct `kernel32` process-tree enumeration and
  termination through `CreateToolhelp32Snapshot`, `OpenProcess`,
  `TerminateProcess`, and `WaitForSingleObject`, without adding a runtime
  dependency.
- [x] (2026-06-25T19:00:27Z) Local validation for the direct Windows reaper
  patch passed before commit. Cross-target checks passed for
  `x86_64-pc-windows-msvc` and `aarch64-apple-darwin` with
  `RUSTFLAGS="-D warnings"` and the CI feature set. The focused Linux
  orphan-detection lifecycle test passed. Required gates passed:
  `make check-fmt`, `make lint`, `make test`, `make markdownlint`, and
  `make nixie`; the full test gate ran `275` tests with `3` skipped and then
  `151` `dev-worker` tests with `0` skipped. Evidence:
  `/tmp/check-windows-direct-reaper-after-ffi-fix-windows-mac-support-validation.out`,
  `/tmp/check-darwin-direct-reaper-windows-mac-support-validation.out`,
  `/tmp/test-shutdown-hook-lifecycle-direct-reaper-windows-mac-support-validation.out`,
  `/tmp/check-fmt-direct-reaper-windows-mac-support-validation.out`,
  `/tmp/lint-direct-reaper-windows-mac-support-validation.out`,
  `/tmp/test-direct-reaper-windows-mac-support-validation.out`,
  `/tmp/mdlint-direct-reaper-windows-mac-support-validation.out`, and
  `/tmp/nixie-direct-reaper-windows-mac-support-validation.out`. An additional
  exploratory Windows-target Clippy run no longer reports this patch's
  `platform.rs` FFI calls after the raw-pointer fix, but it still fails on
  older Windows-target lint findings in unrelated modules, so it is not used as
  a commit gate.
- [x] (2026-06-25T19:10:48Z) CodeRabbit reviewed corrective commit `ba7285b`
  after deterministic gates passed. The first CLI attempt stalled after the
  summarization phase and was stopped by exact process group; the retry also
  appeared stalled, then flushed a completed structured result after the exact
  process group was stopped. The captured log shows `status=review_completed`
  and `findings=0`, so there are no CodeRabbit concerns to clear before pushing
  the direct Windows reaper fix.
- [x] (2026-06-25T19:29:13Z) Remote CI run `28194422551` for commit
  `611e380` proved the direct Win32 reaper still does not satisfy the Windows
  orphan-detection test. Linux root, Linux unprivileged, and macOS all passed.
  Windows built both binaries and passed the CLI smoke test, then failed only in
  `shutdown_hook_lifecycle::postmaster_exits_after_child_process_with_shutdown_hook`:
  the parent observed postmaster PID `2908` still running after the child
  exited and waited `30s`; runner cleanup then terminated orphaned `postgres`
  processes `2908` and `4584`.
- [ ] (2026-06-25T19:29:13Z) User approved continuing past the original
  two-attempt Windows cleanup tolerance with up to four further approaches.
  Approach 3 is a Windows Job Object failsafe: assign the postmaster process
  tree to a `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` job when the shutdown hook is
  registered, so the operating system reaps the tree when the process exits
  even if the `atexit` callback is skipped or cannot complete.
- [x] (2026-06-25T19:38:14Z) Implemented Approach 3 locally by splitting the
  Windows shutdown-hook FFI into
  `src/cluster/shutdown_hook/platform/windows.rs` and
  `src/cluster/shutdown_hook/platform/windows/job.rs`, then retaining a
  kill-on-close Job Object in the registered shutdown state. Local target
  checks pass for `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, and
  `x86_64-apple-darwin` with `RUSTFLAGS="-D warnings"` and the CI feature set.
  The focused Linux shutdown-hook lifecycle test still passes. Evidence:
  `/tmp/check-windows-job-object-after-visibility-windows-mac-support-validation.out`,
  `/tmp/check-darwin-job-object-windows-mac-support-validation.out`,
  `/tmp/check-darwin-intel-job-object-windows-mac-support-validation.out`, and
  `/tmp/test-shutdown-hook-lifecycle-job-object-windows-mac-support-validation.out`.
- [x] (2026-06-25T19:46:38Z) Approach 3 deterministic gates passed locally
  before CodeRabbit review, but the CodeRabbit retry returned the free CLI
  allowance rate limit with a reported `18 minutes and 9 seconds` reset. Per
  the user instruction, pause with `vsleep "$(shuf -i 45-90 -n 1)m"` before
  retrying CodeRabbit rather than proceeding to commit without the review.
  Evidence:
  `/tmp/check-fmt-job-object-after-const-windows-mac-support-validation.out`,
  `/tmp/lint-job-object-after-const-windows-mac-support-validation.out`,
  `/tmp/test-job-object-rerun-windows-mac-support-validation.out`,
  `/tmp/mdlint-job-object-windows-mac-support-validation.out`,
  `/tmp/nixie-job-object-windows-mac-support-validation.out`, and
  `/tmp/coderabbit-job-object-retry-windows-mac-support-validation.out`.
- [x] (2026-06-25T19:54:27Z) Manual review during the CodeRabbit backoff found
  that the first Job Object implementation used `.any(...)` while assigning the
  discovered process tree, which short-circuited after the first successful
  assignment and left later descendants unattempted. The implementation now
  iterates over every discovered PID and records whether at least one
  assignment succeeded.
- [x] (2026-06-25T19:57:16Z) Local validation after the Job Object tree-loop
  fix passed before retrying CodeRabbit. Evidence:
  `/tmp/check-windows-job-object-tree-loop-windows-mac-support-validation.out`,
  `/tmp/check-fmt-job-object-tree-loop-windows-mac-support-validation.out`,
  `/tmp/lint-job-object-tree-loop-windows-mac-support-validation.out`,
  `/tmp/test-job-object-tree-loop-windows-mac-support-validation.out`,
  `/tmp/mdlint-job-object-tree-loop-windows-mac-support-validation.out`, and
  `/tmp/nixie-job-object-tree-loop-windows-mac-support-validation.out`. The
  test gate ran two nextest passes: `275` passed with `3` skipped, then `151`
  passed with `0` skipped.
- [x] (2026-06-25T21:25:55Z) After the mandated backoff completed, three
  CodeRabbit attempts still failed to produce a review result: two full
  `coderabbit review --agent` runs and one
  `coderabbit review --agent --type uncommitted` run all stalled after the
  `summarizing` status and were stopped by their exact process groups. A final
  scoped light attempt, `coderabbit review --agent --light --type uncommitted`,
  returned another free CLI rate limit with a reported
  `49 minutes and 40 seconds` reset. Per the user instruction, pause again with
  `vsleep "$(shuf -i 45-90 -n 1)m"` before retrying CodeRabbit. Evidence:
  `/tmp/coderabbit-job-object-tree-loop-windows-mac-support-validation.out`,
  `/tmp/coderabbit-job-object-tree-loop-retry-windows-mac-support-validation.out`,
  `/tmp/coderabbit-job-object-tree-loop-uncommitted-windows-mac-support-validation.out`,
  and
  `/tmp/coderabbit-job-object-tree-loop-uncommitted-light-windows-mac-support-validation.out`.
- [x] (2026-06-25T23:06:31Z) CodeRabbit's scoped light retry produced one
  `major` finding before the second rate-limit cycle: the Windows cleanup path
  used a bare PID from `postmaster.pid`, so PID reuse could assign or terminate
  an unrelated process. The fix parses the PostgreSQL start timestamp from
  `postmaster.pid`, verifies the live Windows process still has a
  `postgres(.exe)` image name and matching creation time before assigning it to
  the Job Object or terminating its tree, and keeps the public PID-only
  test-support helper feature-gated so docs builds remain warning-free. Local
  validation passed after the fix: Windows target check, `make check-fmt`,
  `make lint`, `make test`, `make markdownlint`, and `make nixie`. Evidence:
  `/tmp/coderabbit-job-object-tree-loop-uncommitted-light-after-backoff-windows-mac-support-validation.out`,
  `/tmp/check-windows-job-object-identity-gated-helpers-windows-mac-support-validation.out`,
  `/tmp/check-fmt-job-object-identity-gated-helpers-windows-mac-support-validation.out`,
  `/tmp/lint-job-object-identity-gated-helpers-windows-mac-support-validation.out`,
  `/tmp/test-job-object-identity-gated-helpers-windows-mac-support-validation.out`,
  `/tmp/mdlint-job-object-identity-gated-helpers-windows-mac-support-validation.out`,
  and
  `/tmp/nixie-job-object-identity-gated-helpers-windows-mac-support-validation.out`.
  The test gate ran two nextest passes: `275` passed with `3` skipped, then
  `151` passed with `0` skipped.
- [x] (2026-06-25T23:13:04Z) CodeRabbit re-reviewed the uncommitted Job
  Object identity-guard patch with
  `coderabbit review --agent --light --type uncommitted` after deterministic
  gates passed. The review completed with `status=review_completed` and
  `findings=0`, so the PID-reuse concern is cleared before committing. Evidence:
  `/tmp/coderabbit-job-object-identity-postfix-windows-mac-support-validation.out`.
- [x] (2026-06-25T23:22:06Z) Remote CI run `28206487696` for commit `de0e441`
  passed all four pull-request jobs. macOS passed in `1m38s`; Windows passed in
  `5m45s` after building both binaries, running the CLI smoke test, and running
  the unprivileged surface tests that include the shutdown-hook
  orphan-detection scenario. Linux root and Linux unprivileged were also green.
  Evidence:
  `https://github.com/leynos/pg-embed-setup-unpriv/actions/runs/28206487696` and
  `/tmp/ci-watch-job-object-identity-windows-mac-support-validation.out`.
- [x] (2026-06-25T23:41:57Z) Implemented the Milestone 3 packaging path
  locally after validating that the pinned shared staging action cannot package
  both production binaries. Added `scripts/release_archive.py`, delegated
  `make release-archive` to that script, extended `release.yml` to publish
  Linux, Windows, Apple Silicon macOS, and Intel macOS `.tgz` archives, and
  added a pull-request `binstall` job that builds a local archive, serves it
  over HTTPS with a throwaway CA, performs a real cargo-binstall install,
  checks both binaries were placed, and runs the installed CLI `--version`.
  Focused local evidence:
  `/tmp/pytest-release-archive-option-fix-windows-mac-support-validation.out`,
  `/tmp/release-archive-linux-option-fix-windows-mac-support-validation.out`,
  `/tmp/archive-list-linux-windows-mac-support-validation.out`, and
  `/tmp/binstall-local-linux-windows-mac-support-validation.out`.
- [x] (2026-06-25T23:52:28Z) Re-ran the deterministic gates for the Milestone 3
  packaging patch before requesting review. Focused script tests passed with
  `5 passed`; `actionlint` passed for the workflow changes; `make check-fmt`,
  `make lint`, `make test`, `make markdownlint`, and `make nixie` all passed.
  The Linux test gate ran the same two nextest passes as the project Makefile:
  `275` tests passed with `3` skipped under all features, then `151` tests
  passed with `0` skipped under the dev-worker feature set. Evidence:
  `/tmp/pytest-release-archive-build-jobs-windows-mac-support-validation.out`,
  `/tmp/actionlint-binstall-packaging-post-script-fix-windows-mac-support-validation.out`,
  `/tmp/check-fmt-binstall-packaging-final-windows-mac-support-validation.out`,
  `/tmp/lint-binstall-packaging-final-windows-mac-support-validation.out`,
  `/tmp/test-binstall-packaging-final-windows-mac-support-validation.out`,
  `/tmp/mdlint-binstall-packaging-final-windows-mac-support-validation.out`, and
  `/tmp/nixie-binstall-packaging-final-windows-mac-support-validation.out`.
- [x] (2026-06-25T23:54:32Z) Requested CodeRabbit review for the uncommitted
  Milestone 3 packaging patch after the deterministic gates above. The scoped
  light review completed with `status=review_completed` and `findings=0`.
  Evidence:
  `/tmp/coderabbit-binstall-packaging-windows-mac-support-validation.out`.
- [x] (2026-06-26T00:02:45Z) Pushed commit `b14b0b7` and observed CI run
  `28208052710`. The existing macOS test job, Windows test job, and Linux root
  job reached green, but the new Linux `Binstall (x86_64-unknown-linux-gnu)`
  job failed in the real install step before downloading the local archive. The
  hosted setup action installed cargo-binstall `1.16.6`, which fails to parse
  this local manifest with `can't load root workspace`; local reproduction with
  a temporary `1.16.6` install produced the same error, while the same archive
  and HTTPS server passed with cargo-binstall `1.19.1`. Patched the packaging
  CI job to install cargo-binstall `1.19.1` into a job-local directory and
  prepend it to `PATH` before the validation install. Evidence:
  `https://github.com/leynos/pg-embed-setup-unpriv/actions/runs/28208052710`,
  `/tmp/ci-binstall-linux-failure-log-api-windows-mac-support-validation.out`,
  `/tmp/cargo-binstall-1.16.6-installs-1.19.1-windows-mac-support-validation.out`,
  and local reproduction output from the temporary `1.16.6` run.
- [x] (2026-06-26T00:06:54Z) Pulled the remaining failed jobs from the same
  hosted run before committing the cargo-binstall version fix. macOS failed in
  the certificate-signing step because its OpenSSL does not support
  `openssl x509 -copy_extensions copy`; the CI script now writes a portable
  `server-ext.cnf` and signs the server certificate with `-extfile`. Windows
  failed while building the release archive because a release binary build
  without test-support features exposed unused Windows shutdown-hook helper
  imports under `-D warnings`; the Windows PID/probe re-exports and the probe
  function are now compiled only when the test-support API is compiled. Local
  `cargo check --target x86_64-pc-windows-msvc --release --bin pg_embedded_setup_unpriv --bin pg_worker`
  and `actionlint` passed after those fixes. Evidence:
  `/tmp/ci-binstall-macos-failure-log-api-windows-mac-support-validation.out`,
  `/tmp/ci-binstall-windows-failure-log-api-windows-mac-support-validation.out`,
  `/tmp/check-windows-release-binstall-fixes-windows-mac-support-validation.out`,
  and
  `/tmp/actionlint-binstall-packaging-portable-openssl-windows-mac-support-validation.out`.
- [x] (2026-06-26T00:04:22Z) Re-ran deterministic checks for the
  cargo-binstall version fix before review: `actionlint`, `make markdownlint`,
  `make nixie`, and `git diff --check` passed. CodeRabbit then reviewed the
  uncommitted workflow/doc patch with
  `coderabbit review --agent --light --type uncommitted` and completed with
  `status=review_completed` and `findings=0`. Evidence:
  `/tmp/actionlint-binstall-packaging-binstall-version-fix-windows-mac-support-validation.out`,
  `/tmp/mdlint-binstall-packaging-binstall-version-fix-windows-mac-support-validation.out`,
  `/tmp/nixie-binstall-packaging-binstall-version-fix-windows-mac-support-validation.out`,
  and
  `/tmp/coderabbit-binstall-version-fix-windows-mac-support-validation.out`.
- [x] (2026-06-26T00:08:55Z) Re-ran the full local gate set after the combined
  Linux/macOS/Windows packaging CI fixes.
  `cargo check --target x86_64-pc-windows-msvc --release --bin pg_embedded_setup_unpriv --bin pg_worker`,
  `actionlint`, `make check-fmt`, `make lint`, `make test`,
  `make markdownlint`, `make nixie`, and `git diff --check` passed. The Linux
  test gate again ran two nextest passes: `275` tests passed with `3` skipped,
  then `151` tests passed with `0` skipped. Evidence:
  `/tmp/check-windows-release-binstall-fixes-windows-mac-support-validation.out`,
  `/tmp/actionlint-binstall-packaging-portable-openssl-windows-mac-support-validation.out`,
  `/tmp/check-fmt-binstall-packaging-ci-fixes-windows-mac-support-validation.out`,
  `/tmp/lint-binstall-packaging-ci-fixes-windows-mac-support-validation.out`,
  `/tmp/test-binstall-packaging-ci-fixes-windows-mac-support-validation.out`,
  `/tmp/mdlint-binstall-packaging-ci-fixes-windows-mac-support-validation.out`,
  and
  `/tmp/nixie-binstall-packaging-ci-fixes-windows-mac-support-validation.out`.
- [x] (2026-06-26T01:28:12Z) CodeRabbit reviewed the combined
  Linux/macOS/Windows packaging CI fixes after the full local gate set passed
  and after the required randomised backoff for the free CLI rate limit.
  `coderabbit review --agent --light --type uncommitted` completed with
  `status=review_completed` and `findings=0`, so there are no CodeRabbit
  concerns to clear before committing and pushing the corrective patch.
  Evidence:
  `/tmp/coderabbit-binstall-ci-fixes-retry-windows-mac-support-validation.out`.
- [x] (2026-06-26T01:40:48Z) Pushed commit `af35126` and observed CI run
  `28211320490`. The GitHub MCP workflow-read tool was tried first, but its
  authentication token is expired, so CI observation continues through the
  authenticated `gh` CLI until that connector is repaired. Linux root, Linux
  unprivileged, macOS tests, Windows tests, and the Linux real `binstall` job
  all passed. The macOS `binstall` job failed while bootstrapping cargo-binstall
  `1.19.1`: source fallback replaced the Cargo-home `cargo-binstall` binary
  instead of writing to the requested `$RUNNER_TEMP/cargo-binstall-1.19.1`
  directory. The Windows `binstall` job built the archive but failed when Git
  Bash rewrote OpenSSL's `/CN=...` certificate subject into a
  `C:/Program Files/Git/...` path. Evidence:
  `https://github.com/leynos/pg-embed-setup-unpriv/actions/runs/28211320490`,
  `/tmp/ci-binstall-macos-af35126-job-logs-api-windows-mac-support-validation.out`,
  `/tmp/ci-binstall-windows-af35126-job-logs-api-windows-mac-support-validation.out`,
  and `/tmp/gh-watch-28211320490-windows-mac-support-validation.out`.
- [x] (2026-06-26T01:40:48Z) Applied the next local `binstall` workflow patch:
  after installing cargo-binstall `1.19.1`, resolve the actual binary from
  either the requested job-local directory or `${CARGO_HOME:-$HOME/.cargo}/bin`
  and verify the version before adding it to `PATH`; for the throwaway HTTPS
  certificates, set `MSYS_NO_PATHCONV=1` only on the OpenSSL commands that pass
  `/CN=...` subjects. Deterministic gates passed before review: `actionlint`,
  `make check-fmt`, `make lint`, `make test`, `make markdownlint`,
  `make nixie`, and `git diff --check`. Evidence:
  `/tmp/actionlint-binstall-pathconv-fallback-windows-mac-support-validation.out`,
  `/tmp/check-fmt-binstall-pathconv-fallback-windows-mac-support-validation.out`,
  `/tmp/lint-binstall-pathconv-fallback-windows-mac-support-validation.out`,
  `/tmp/test-binstall-pathconv-fallback-windows-mac-support-validation.out`,
  `/tmp/mdlint-binstall-pathconv-fallback-windows-mac-support-validation.out`,
  `/tmp/nixie-binstall-pathconv-fallback-windows-mac-support-validation.out`,
  and
  `/tmp/diff-check-binstall-pathconv-fallback-final-windows-mac-support-validation.out`.
- [x] (2026-06-26T01:43:09Z) CodeRabbit reviewed the uncommitted
  cargo-binstall path-resolution and Windows OpenSSL path-conversion patch
  after deterministic gates passed.
  `coderabbit review --agent --light --type uncommitted` completed with
  `status=review_completed` and `findings=0`. Evidence:
  `/tmp/coderabbit-binstall-pathconv-fallback-windows-mac-support-validation.out`.
- [x] (2026-06-26T01:55:29Z) Pushed commit `e963d04` and observed CI run
  `28211894514`. Linux root, Linux unprivileged, macOS tests, Windows tests,
  and the Linux real `binstall` job all passed. The macOS and Windows
  `binstall` jobs both reached the local HTTPS readiness loop after building
  their release archives, then failed before `cargo-binstall` ran: macOS curl
  rejected the throwaway CA with `unable to get local issuer certificate`, and
  Windows curl/SChannel rejected it with `the revocation status is unknown`.
  Evidence:
  `https://github.com/leynos/pg-embed-setup-unpriv/actions/runs/28211894514`,
  `/tmp/ci-binstall-macos-e963d04-job-logs-api-windows-mac-support-validation.out`,
  `/tmp/ci-binstall-windows-e963d04-job-logs-api-windows-mac-support-validation.out`,
  and `/tmp/gh-watch-28211894514-windows-mac-support-validation.out`.
- [x] (2026-06-26T01:58:05Z) Applied the next local `binstall` workflow patch:
  make the curl readiness probe use `--insecure` because it only waits for the
  local HTTPS server to accept connections; keep the
  `--root-certificates "$cert_dir/ca.pem"` argument on `cargo binstall`
  unchanged as the actual CA-trust validation. Deterministic gates passed
  before review: `actionlint`, `make check-fmt`, `make lint`, `make test`,
  `make markdownlint`, `make nixie`, and `git diff --check`. CodeRabbit then
  reviewed the uncommitted patch with
  `coderabbit review --agent --light --type uncommitted` and completed with
  `status=review_completed` and `findings=0`. Evidence:
  `/tmp/actionlint-binstall-readiness-curl-windows-mac-support-validation.out`,
  `/tmp/check-fmt-binstall-readiness-curl-windows-mac-support-validation.out`,
  `/tmp/lint-binstall-readiness-curl-windows-mac-support-validation.out`,
  `/tmp/test-binstall-readiness-curl-windows-mac-support-validation.out`,
  `/tmp/mdlint-binstall-readiness-curl-windows-mac-support-validation.out`,
  `/tmp/nixie-binstall-readiness-curl-windows-mac-support-validation.out`,
  `/tmp/diff-check-binstall-readiness-curl-windows-mac-support-validation.out`,
  and
  `/tmp/coderabbit-binstall-readiness-curl-windows-mac-support-validation.out`.
- [x] (2026-06-26T02:10:43Z) Pushed commit `35a6bbd` and observed CI run
  `28212300565`. Linux root, Linux unprivileged, macOS tests, Windows tests,
  Linux `binstall`, and Windows `binstall` all passed. The macOS `binstall` job
  reached the real `cargo-binstall` install step and failed while validating
  the local HTTPS archive: `rustls-platform-verifier`/Apple Security rejected
  the extra root certificate with
  `"pg local test CA" certificate is not standards compliant: -67903`. The
  GitHub MCP workflow-log tool was tried first again, but the connector token
  remains expired, so logs were fetched with the authenticated `gh` CLI.
  Evidence:
  `https://github.com/leynos/pg-embed-setup-unpriv/actions/runs/28212300565`,
  `/tmp/ci-binstall-macos-35a6bbd-job-logs-api-windows-mac-support-validation.out`,
  and `/tmp/gh-watch-28212300565-windows-mac-support-validation.out`.
- [x] (2026-06-26T02:14:28Z) User approved continuing with four more approaches
  for the remaining macOS `binstall` validation failure. Approach 1 replaces
  the ad hoc one-line OpenSSL root/server certificate generation with explicit
  config files, a non-empty organisation in both subjects, fixed short serial
  numbers, explicit CA/server X.509v3 extensions, and an
  `openssl verify -x509_strict -purpose sslserver` check before the local HTTPS
  server starts. Local Linux real-install validation still passes with
  cargo-binstall `1.19.1` and the new certificate profile. Deterministic gates
  passed before CodeRabbit review: `actionlint`, `make fmt`, `make check-fmt`,
  `make lint`, `make test`, `make markdownlint`, `make nixie`, and
  `git diff --check`. The first CodeRabbit attempt hit the free CLI rate limit,
  so the mandated randomised backoff ran for `52` minutes before retrying. The
  retry completed with `status=review_completed` and `findings=0`. Evidence:
  `/tmp/actionlint-binstall-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/explicit-cert-strict-verify-windows-mac-support-validation.out`,
  `/tmp/release-archive-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/binstall-local-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/fmt-binstall-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/check-fmt-binstall-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/lint-binstall-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/test-binstall-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/mdlint-binstall-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/nixie-binstall-explicit-cert-windows-mac-support-validation.out`, and
  `/tmp/diff-check-binstall-explicit-cert-windows-mac-support-validation.out`,
  `/tmp/coderabbit-binstall-explicit-cert-windows-mac-support-validation.out`,
  and
  `/tmp/coderabbit-binstall-explicit-cert-retry-windows-mac-support-validation.out`.
- [x] Milestone 1: make the library and both binaries compile on Windows and
  macOS (`fs.rs` mode gating; `nix` target-gating; `tests/` `nix` import
  gating; remove the dead `xdg` dependency; resolve `openssl-sys`), AND resolve
  the Windows shared-cluster cleanup question with an orphan-detection test
  (Red → Green). Exit gate includes one real Windows link-and-run.
- [x] Milestone 2: add a test-only macOS and Windows CI matrix (tests +
  orphan-detection), with mandatory theseus caching and a recorded cost budget;
  observe it pass.
- [x] Milestone 3: add `binstall` packaging for macOS and Windows; extend the
  release workflow to build and upload the new archives.
- [ ] Milestone 4: validate `binstall` with a real install-and-run per OS at
  pull-request time and an end-to-end check against real assets at release time.
- [ ] Milestone 5: update README, users' guide, roadmap appendix (and close
  item 3.3.2), and the design doc; run all commit gateways; finalise.

## Surprises & discoveries

- Observation: the unprivileged code path is already largely portable, and more
  of the platform-gating is already done than a first survey suggested. The
  `atexit` shutdown hook is already gated: `src/cluster/mod.rs:61` reads
  `#[cfg(unix)] mod shutdown_hook;`, the re-exports at lines 63-67 are gated,
  and `src/cluster/handle.rs:312-330` already has a `#[cfg(unix)]` real
  implementation plus a `#[cfg(not(unix))]` no-op. Evidence: direct reads.
  Impact: the earlier claim that `shutdown_hook` is the "primary Windows
  compile blocker" was wrong; there is nothing to port for compilation. The
  remaining questions are the genuine compile blockers (below) and the
  *behavioural* gap that the non-Unix no-op leaves shared clusters unreaped.
- Observation: the genuine unconditional Windows compile blockers are
  `src/fs.rs` (cap-std `PermissionsExt`/`from_mode`), the unconditional `nix`
  dependency, the unconditional `use nix::unistd::geteuid;` in
  `tests/settings.rs:7` (compiled by `--all-targets`), and the apparently-dead
  Unix-only `xdg` crate. Evidence: `src/fs.rs:7,159`; `Cargo.toml:115,125`;
  `tests/settings.rs:7`; grep showing no `xdg::` usage. Impact: these define
  Milestone 1's real scope; the original survey covered `src/` only and missed
  the `tests/` tree and the dead dependency.
- Observation: the roadmap appendix and `docs/users-guide.md` already specify
  the intended macOS (unprivileged in-process; root fails fast) and Windows
  (always unprivileged) behaviour, and roadmap item 3.3.2 ("guardrails that
  fail fast on unsupported root scenarios on non-Linux systems") is still open.
  Evidence: `docs/roadmap.md` lines 50-85; `docs/users-guide.md` lines 20-45.
  Impact: this plan implements and validates that documented intent and can
  close 3.3.2.
- Observation: `leynos/shared-actions` already provides tri-platform building
  blocks — `setup-rust`, `rust-build-release`, and `stage-release-artefacts`
  support Windows and macOS; `rust-toy-app.yml` is the canonical cross-OS
  matrix reference; `upload-codescene-coverage` is POSIX-only with no Windows
  path. The current default-branch HEAD is
  `7da7c6d89033d13cbb1c64803d108ddca97e69c2`; the repo is pinned at
  `b61ad14766ae81b830abdaf066eb3d9b6c4f537b`. Evidence: inspection of the
  repository's actions and workflows. Impact: prefer reusing these actions, but
  the later Milestone 3 implementation supersedes the `stage-release-artefacts`
  packaging recommendation because the pinned action packages one binary.
- Observation (from the design-review panel): a `--dry-run` `binstall` check
  does not extract the archive, so it cannot catch a wrong internal layout; and
  the release runner could build the archive via a different code path than the
  PR-time check, so the artefact that ships may not be the one tested. Impact:
  validation must include a real install-and-run, and local/release archive
  production should share one code path.
- Observation: Milestone 0's three current-HEAD cross-target checks all fail
  before this crate's Rust source is typechecked because `openssl-sys` is an
  unconditional direct dependency with `features = ["vendored"]`. On
  `x86_64-apple-darwin` and `aarch64-apple-darwin`, the Linux host compiler
  rejects Darwin flags such as `-arch` and `-mmacosx-version-min`; on
  `x86_64-pc-windows-msvc`, OpenSSL's configure step rejects the Linux Perl
  because it does not produce Windows-style paths. Evidence:
  `/tmp/check-darwin-windows-mac-support-validation.out`,
  `/tmp/check-darwin-arm-windows-mac-support-validation.out`, and
  `/tmp/check-windows-windows-mac-support-validation.out`. Impact: the
  source-level `fs.rs`/`nix` errors are currently masked; dependency
  portability must be fixed first and the cross-target checks rerun.
- Observation: `cargo tree --target x86_64-pc-windows-msvc -i openssl-sys`
  shows two routes into `openssl-sys`: the crate's direct dependency and the
  default `postgresql_embedded` native-TLS feature.
  `cargo info postgresql_embedded@0.20.2` shows a supported `rustls` feature,
  and `postgresql_archive@0.20.2` exposes matching `rustls` support. Impact:
  the first implementation attempt disabled default `postgresql_embedded`
  features and opted into `tokio`, `theseus`, and `rustls`, but that path was
  later rejected after direct validation.
- Observation: the `rustls` route pulls in `ring`/`aws-lc-sys` for this
  dependency graph, and the Linux-hosted Windows cross-check then needs MSVC
  tools such as `lib.exe` and NASM. This violates the dependency tolerance that
  no new build tool should be required for portability validation. Evidence:
  the failed intermediate `x86_64-pc-windows-msvc` check after enabling
  `postgresql_embedded`'s `rustls` feature. Impact: keep
  `postgresql_embedded`'s default native TLS backend and remove only this
  crate's direct vendored OpenSSL dependency.
- Observation: after removing the direct vendored `openssl-sys` dependency, the
  native-TLS target graph is portable for the required checks: Windows uses the
  platform TLS backend, macOS uses Security.framework, and Linux keeps its
  native OpenSSL path. The three cross-target checks and the same checks with
  `RUSTFLAGS="-D warnings"` all pass. Impact: no new runtime dependency or
  runner build-tool provisioning is needed for Milestone 1 compilation.
- Observation: `tests/cluster_split_constructors.rs` and
  `tests/recovery_integration.rs` existed as integration tests without explicit
  `[[test]]` entries, so Cargo treated them as default tests during
  `--all-targets` cross-checks instead of applying their intended feature
  scopes. Impact: add explicit `[[test]]` entries with
  `required-features = ["cluster-unit-tests"]` and
  `required-features = ["dev-worker"]` respectively, matching the feature gates
  used by neighbouring integration tests.
- Observation: making Unix-only `worker_env_tests` visible to the Linux
  all-feature lint pass exposed an existing `expect()`-based `rstest` fixture
  setup that violated the repository's `clippy::expect_used` policy. Impact:
  convert that fixture setup and the two affected tests to return
  `color_eyre::Result` and use `ensure!`/`?`, so setup and assertion failures
  report errors without panicking.
- Observation: a literal audit found that `src/cache/lock.rs` and
  `src/cache/operations/copy.rs` already contain Unix/non-Unix implementations,
  so their Unix imports are not new blockers. The top-level `xdg` crate remains
  unused; the only matches are `Cargo.toml`, this ExecPlan, and local `xdg_*`
  path variable/function names. Evidence:
  `rg "PermissionsExt|from_mode|nix::|std::os::unix|libc::|xdg" src tests` and
  `rg "xdg::|use xdg|xdg ="`. Impact: Milestone 1 keeps those existing cache
  splits and focuses on dependency gating plus the unguarded filesystem mode
  helper.
- Observation: `libc::atexit` is available on Windows through the existing
  `libc` dependency, so the process-exit registration mechanism does not
  require a new runtime crate. Windows still needs different process controls:
  POSIX signals cannot terminate the postmaster tree there. Impact: the Windows
  implementation keeps the existing atexit shape, uses the native `taskkill`
  utility for process-tree termination, and limits raw Win32 FFI to liveness
  checks.
- Observation: making the orphan-detection parent test run by default exposed a
  deterministic nextest contention point. The focused lifecycle test passed,
  but the first full `make test` run failed because another cluster test was
  using the shared `/var/tmp/pg-embed-1000/data` directory while the child
  process tried to start. Impact: the shutdown-hook registration and lifecycle
  integration tests must hold the existing `tests/support/serial.rs`
  cross-process scenario guard before creating a real `TestCluster`.
- Observation: `tests/support/serial.rs` previously provided only an in-process
  mutex on non-Unix targets. That would leave Windows nextest runs exposed to
  the same cross-binary cluster-directory race seen locally on Linux before the
  shutdown-hook tests acquired the guard. Impact: non-Unix test runs now use an
  atomic lock directory under `CARGO_TARGET_DIR` for cross-process coordination
  without adding a dependency.
- Observation: `PgEnvCfg` only pins a `postgresql_embedded` version when
  `PG_VERSION_REQ` is supplied; otherwise the backend resolves the default
  requirement at runtime. Impact: the cross-platform CI legs set
  `PG_VERSION_REQ="=17.4.0"` so the mandatory PostgreSQL binary cache key is
  stable and scoped to one known upstream binary version.
- Observation: `actions/cache` tag v4.3.0 currently resolves to
  `0057852bfaa89a56745cba8c7296529d2fc39830`, and `leynos/shared-actions`
  default-branch HEAD still resolves to
  `7da7c6d89033d13cbb1c64803d108ddca97e69c2`. Impact: the workflow can satisfy
  the repository's SHA pinning policy without drifting from the planned
  shared-actions revision.
- Observation: remote macOS CI compiles the test harness for the
  `cluster-unit-tests,async-api` feature set before running any tests, so a
  root-only Linux/BSD helper import fails even when the corresponding scenario
  would have skipped at runtime. Impact: root-specific test binaries and helper
  imports must be target-gated at compile time, not just skipped dynamically.
- Observation: remote macOS CI reached the unprivileged test runtime in run
  `28179624023`; the remaining deterministic macOS failure was not in worker
  discovery itself, but in the Unix test fixture creating a filename from raw
  bytes `0xff, 0xfe, 0xfd`. APFS rejects that pathname with "Illegal byte
  sequence" before `discover_worker_from_path_value` runs. Impact: the
  non-UTF-8 PATH-entry fixture is only portable to Unix filesystems that allow
  arbitrary non-NUL bytes in path components; macOS must retain coverage
  through other worker-discovery tests unless a different fixture is designed.
- Observation: the same run's Windows leg failed in the binary build step while
  downloading `cap-primitives` from crates.io, with a connection reset before
  crate compilation. Impact: this is external network noise; rerun failed
  workflow jobs after the run completes before making code changes for Windows.
- Observation: remote CI run `28185956153` showed both cross-platform test legs
  now reach runtime after building the regular binaries and running the CLI
  smoke test. The remaining macOS failure was a Linux/BSD root-helper
  assertion: its check that the system temp directory is world-executable is
  required for `nobody` traversal, not for macOS unprivileged support. The
  remaining Windows failure was an assertion bug in `src/cache/config.rs`
  tests: the product joins `XDG_CACHE_HOME` and `CACHE_SUBDIR` through platform
  path APIs, so Windows naturally renders `\` separators. Impact: treat these
  as test corrections, not source-portability changes.
- Observation: remote CI run `28192593707` proved the first Windows reaper
  shape was insufficient. The Windows job reached the orphan-detection test
  after successful binary build and CLI smoke, but `taskkill /PID <pid> /T`
  from the atexit hook did not stop the postmaster tree: the parent timed out
  after `30s`, and GitHub runner cleanup later killed two orphaned `postgres`
  processes. Impact: spawning `taskkill` inside the process-exit path is not a
  reliable cleanup primitive for this test cluster; the Windows hook must use
  direct process handles and kill descendants itself.
- Observation: remote CI run `28194422551` proved direct
  `TerminateProcess` calls from the Windows atexit path were also insufficient.
  The failure shape was unchanged: the postmaster PID from `postmaster.pid`
  stayed alive for the parent process's full `30s` polling window, and the
  GitHub runner later cleaned up the same `postgres` process tree. Impact: the
  next approach must not depend solely on the atexit callback reaching and
  terminating the postmaster during process teardown.
- Observation: the first local Job Object patch used `.any(...)` to assign the
  discovered Windows process tree to the kill-on-close job. Because `.any(...)`
  short-circuits on the first success, a successful root-process assignment
  prevented any later descendants from being attempted. Impact: assigning the
  tree must be expressed as an explicit loop that visits every PID and records
  whether at least one assignment succeeded.
- Observation: CodeRabbit found that the Job Object and forced-termination
  paths must not trust a bare PID from `postmaster.pid`, because Windows may
  reuse that PID for an unrelated process before cleanup runs. Impact: the
  Windows shutdown hook now treats the PID and PostgreSQL start timestamp as a
  pair, then verifies the live process image and creation time before assigning
  or terminating the process tree.
- Observation: remote CI run `28206487696` proved the Job Object failsafe with
  PID-reuse protection satisfies the hosted Windows orphan-detection scenario.
  The Windows job built both binaries, passed the CLI smoke test, ran the
  unprivileged surface tests, and completed without runner cleanup reporting
  orphaned PostgreSQL processes. Impact: Approach 3 is the accepted Windows
  cleanup implementation; no fourth cleanup approach is needed before moving to
  `binstall` packaging.
- Observation: the pinned
  `leynos/shared-actions/stage-release-artefacts` implementation stages a single
  `binary_source` into one cargo-binstall archive and emits tar.gz archives
  only. The crate has two production binaries, and `cargo binstall` installs
  all binaries by default when `--bin` is omitted. Impact: using that action
  as-is would validate and publish an incomplete install, so Milestone 3 uses a
  repo-local packager instead.
- Observation: this cargo-binstall build rejects local `http://` and `file://`
  package URLs with `BadScheme`. A pull-request-time real install can still be
  tested without a GitHub release by serving the archive over local HTTPS with
  a throwaway CA and passing that CA via `--root-certificates`. Impact: the CI
  `binstall` job uses a local HTTPS server rather than a plain Python
  `http.server`.
- Observation: cargo-binstall `1.16.6`, the version installed by the pinned
  shared setup action at implementation time, cannot parse this crate through
  `--manifest-path Cargo.toml` for the local cargo-binstall validation path; it
  reports `can't load root workspace` before trying the local package URL. The
  same archive and command shape work with cargo-binstall `1.19.1`, and
  `1.16.6` can bootstrap `1.19.1` into an isolated install directory. Impact:
  the `binstall-packaging` job must pin the cargo-binstall version it uses for
  validation instead of inheriting the shared action's bundled version.
- Observation: macOS's hosted OpenSSL accepts `openssl req -addext` in the
  request step but rejects `openssl x509 -copy_extensions copy` in the signing
  step. Impact: certificate extensions for the local HTTPS server must be
  supplied through an explicit `-extfile` that works across Linux, macOS, and
  Windows OpenSSL builds.
- Observation: `make lint` and the cross-platform test jobs compile Windows
  with test-support features, but the release archive builder compiles the
  production binaries without those features. That narrower production build
  exposed unused Windows shutdown-hook PID helper re-exports that the earlier
  all-feature gates could not see. Impact: Milestone 3 needs a release-target
  Windows check as part of its packaging evidence, not only all-targets test
  checks.
- Observation: the GitHub MCP workflow-read tool is currently unusable in this
  session because the connector returns an expired-token error. Impact: use the
  authenticated `gh` CLI for CI observation and raw job-log retrieval until the
  connector token is refreshed; keep the MCP failure recorded because the
  user's preferred validation route was attempted.
- Observation: on the hosted Apple Silicon macOS runner, bootstrapping
  cargo-binstall `1.19.1` from cargo-binstall `1.16.6` may fall back to a
  source build and replace the existing Cargo-home `cargo-binstall` executable
  rather than honouring the requested `--install-path`. Impact: the CI job must
  discover the effective installed binary path after bootstrap and verify its
  version, instead of assuming `$RUNNER_TEMP/cargo-binstall-1.19.1` contains
  the executable.
- Observation: on the hosted Windows runner, Git Bash/MSYS path conversion
  rewrites OpenSSL certificate subject arguments such as `/CN=pg local test CA`
  into paths rooted under `C:/Program Files/Git/`, which OpenSSL rejects as an
  invalid subject name. Impact: the workflow must disable MSYS path conversion
  for the OpenSSL invocations that pass slash-prefixed certificate subjects.
- Observation: the hosted macOS and Windows `binstall` runners both reject the
  throwaway local CA during the curl readiness probe, but in platform-specific
  ways: macOS curl reports `unable to get local issuer certificate`, while
  Windows curl backed by SChannel reports `the revocation status is unknown`.
  In both cases the local HTTPS server is already accepting connections and the
  failure happens before `cargo-binstall` can run. Impact: the readiness probe
  should only test server availability; the cargo-binstall command below it
  remains the cross-platform CA-validation gate through `--root-certificates`.
- Observation: after the readiness-probe fix, CI run `28212300565` proved the
  remaining `binstall` failure is macOS-specific and happens inside
  `cargo-binstall`'s Rust TLS stack. Windows `binstall` passed the same local
  HTTPS validation, while macOS failed with Apple Security error `-67903`,
  reported by `rustls-platform-verifier` as the extra root certificate not
  being standards compliant. Local `openssl verify -x509_strict` accepts the
  original chain, so this is a platform verifier policy difference rather than
  a plain OpenSSL chain-building failure. Impact: first try a more explicit
  root/server certificate profile; if that still fails, stop relying on a
  custom CA as the macOS PR-time validation transport.

## Decision log

- Decision: re-ground the entire library-portability analysis against current
  HEAD before implementing Milestone 1, treating the Risk register as a
  hypothesis list until Milestone 0's real cross-compile confirms it.
  Rationale: the first survey mis-read the `shutdown_hook` gating; confidence
  levels were asserted without a real `cargo check --target`. Date/Author:
  2026-06-25, planning agent (incorporating Logisphere review).
- Decision: there is no Windows `shutdown_hook` *compilation* work; instead
  treat Windows shared-cluster cleanup as a *behavioural* task — decide reaper
  vs. prove-no-leak, validated by an orphan-detection test. Rationale: the
  module is already `cfg(unix)`-gated with a non-Unix no-op; the real risk is
  an orphaned postmaster, not a build failure. Date/Author: 2026-06-25,
  planning agent.
- Decision (superseded after validation): prefer
  `leynos/shared-actions/stage-release-artefacts` (and the `rust-toy-app.yml`
  matrix shape) as the primary packaging and cross-OS path; demote any
  hand-rolled Makefile `.zip`/`.exe` branching to a documented
  local-development fallback. Rationale at planning time: the user nominated
  `shared-actions` as the reference; the action appeared to handle Windows
  archive layout and a `.sha256` sidecar. Superseded because implementation
  inspection showed the pinned action packages one binary only. Date/Author:
  2026-06-25, planning agent (incorporating review); superseded 2026-06-25,
  implementation agent.
- Decision: use `scripts/release_archive.py` as the single archive builder for
  release and pull-request packaging validation, with `make release-archive`
  delegating to it for local development. Rationale: the script is the smallest
  cross-platform implementation that can build both production binaries, stage
  `.exe` names for Windows, and create the cargo-binstall `.tgz` archive
  without relying on GNU Make or `tar` being present on Windows. The script
  follows the plan's helper-script constraints by using a `uv` inline metadata
  block, Python 3.13+, `cyclopts`, `cuprum`, `pathlib`, and tests under
  `scripts/tests/` using `pytest` plus `cmd-mox`. Date/Author: 2026-06-25,
  implementation agent.
- Decision: validate `binstall` primarily by a real install-and-run
  (`cargo binstall --install-path <tmp>` then execute `--version`) on each OS,
  with `--dry-run` per-target resolution as a secondary breadth check.
  Rationale: `--dry-run` cannot catch extraction/placement faults. Date/Author:
  2026-06-25, planning agent.
- Decision: install cargo-binstall `1.19.1` into `$RUNNER_TEMP` in the
  `binstall-packaging` job and prepend that directory to `PATH` before running
  the validation install. Rationale: the shared action's bundled cargo-binstall
  `1.16.6` fails on this local-manifest validation path; the newer tool version
  is isolated to the packaging job and can be installed by the older tool
  before the crate-under-test is validated. Date/Author: 2026-06-26,
  implementation agent.
- Decision: after bootstrapping cargo-binstall `1.19.1`, accept either the
  requested job-local install directory or Cargo home's `bin` directory as the
  effective tool location, then verify that `cargo-binstall -V` reports
  `1.19.1` before adding that directory to `PATH`. Rationale: hosted macOS can
  source-build the tool and replace the Cargo-home executable despite
  `--install-path`, so version verification is the stable contract.
  Date/Author: 2026-06-26, implementation agent.
- Decision: sign the local HTTPS server certificate with a generated OpenSSL
  extension file instead of relying on `openssl x509 -copy_extensions`.
  Rationale: Linux accepted `-copy_extensions`, but the hosted macOS OpenSSL
  rejected it; an explicit `subjectAltName`/`serverAuth` `-extfile` keeps the
  throwaway CA validation portable. Date/Author: 2026-06-26, implementation
  agent.
- Decision: set `MSYS_NO_PATHCONV=1` only for the OpenSSL commands that pass
  `/CN=...` certificate subjects in the Windows `binstall` validation job.
  Rationale: this prevents Git Bash from rewriting certificate subjects while
  leaving ordinary path conversion available for the rest of the cargo-binstall
  install step. Date/Author: 2026-06-26, implementation agent.
- Decision: use `curl --insecure` only in the local HTTPS readiness loop, not
  in the actual `cargo-binstall` install. Rationale: the loop's contract is to
  wait until the throwaway server accepts HTTPS connections; trust validation
  belongs to the subsequent `cargo binstall --root-certificates` command, and
  hosted macOS/Windows curl backends reject the generated one-day CA before
  that command can exercise the real install path. Date/Author: 2026-06-26,
  implementation agent.
- Decision: continue past the original `binstall` validation tolerance after
  explicit user approval on 2026-06-26. Try up to four further approaches,
  recording each in this ExecPlan and validating through deterministic local
  gates plus hosted CI. Rationale: the original tolerance correctly forced
  escalation after the primary and alternate `binstall` validation attempts;
  the user has now supplied the required direction to keep iterating.
  Date/Author: 2026-06-26, implementation agent.
- Decision: make the first resumed macOS `binstall` approach a more explicit
  throwaway certificate profile rather than weakening TLS validation.
  Rationale: the latest hosted run proves Windows can install from the local
  HTTPS archive and macOS reaches `cargo-binstall`; the failing component is
  Apple Security's acceptance of the extra root anchor, so the least invasive
  next step is to generate CA and server certificates with explicit X.509v3
  extensions, fixed short serials, and a fuller subject identity. Date/Author:
  2026-06-26, implementation agent.
- Decision: do not add a bespoke Python `binstall` self-test unless the
  reuse-first evaluation shows the shared action plus a small Rust/CI check is
  insufficient; if one is added, it follows the df12 scripting standards.
  Rationale: importing a full Python (uv/cyclopts/cuprum/cmd-mox) substrate
  into a Rust+Make repo to assert an archive listing is disproportionate for a
  small maintainer team, and a renderer that re-implements the `binstall`
  template grammar can agree with itself while disagreeing with the real
  resolver. Date/Author: 2026-06-25, planning agent (incorporating review).
- Decision: target `x86_64-pc-windows-msvc` for Windows and both
  `aarch64-apple-darwin` and `x86_64-apple-darwin` for macOS; do not target
  `aarch64-pc-windows-msvc`. Rationale: MSVC is the standard distributable
  Windows ABI with upstream PostgreSQL binaries; theseus publishes both macOS
  arches; theseus publishes no arm64 Windows PostgreSQL binaries. Date/Author:
  2026-06-25, planning agent.
- Decision: macOS and Windows CI legs run the unprivileged in-process path and
  tests only; linting, formatting, Markdown lint, coverage, and CodeScene
  upload stay on a single Linux job. Rationale: the user's requirement ("the
  Windows and Mac CI branches should run tests only"); lint/format results are
  platform-independent; `upload-codescene-coverage` has no Windows path.
  Date/Author: 2026-06-25, planning agent.
- Decision: keep `pkg-fmt = "tgz"` for Linux, macOS, and Windows; do not add a
  Windows `zip` override. Rationale: cargo-binstall supports `tgz` on Windows,
  the existing `bin-dir` template already appends `{ binary-ext }`, and one
  archive format keeps both production binaries in a single package.
  Date/Author: 2026-06-25, implementation agent.
- Decision: bump the `leynos/shared-actions` pin to the current default-branch
  HEAD `7da7c6d89033d13cbb1c64803d108ddca97e69c2` across all workflows.
  Rationale: a clean fast-forward that picks up the tri-platform actions; keep
  one consistent pin. (Verify the SHA at implementation time; if it has
  advanced, pin to the then-current HEAD and note it here.) Date/Author:
  2026-06-25, planning agent.
- Decision (recorded, not adopted): a viable alternative is library-only
  Windows/macOS support with no `binstall` binaries, dropping Milestones 3-4.
  Rejected because the user explicitly requested `cargo binstall` support for
  both platforms; kept on record because the CLI's headline value (privilege
  drop) is Linux-only, so if that requirement is ever relaxed this is the
  80/20. Date/Author: 2026-06-25, planning agent (Wafflecat alternative).
- Decision: proceed from draft to implementation on 2026-06-25 after the user
  explicitly requested implementation of this plan. Rationale: the execplans
  workflow requires approval before implementation; the user's request supplies
  that approval and also requires the plan to remain current during execution.
  Date/Author: 2026-06-25, implementation agent.
- Decision (rejected after validation): resolve OpenSSL portability by switching
  `postgresql_embedded` from default native TLS to explicit `rustls` support.
  Rationale for trying it: Milestone 0 proved vendored OpenSSL blocks all three
  target checks before source diagnostics, and `postgresql_embedded`/
  `postgresql_archive` both expose `rustls` feature flags for the same archive
  download path. Rejection rationale: the resulting `ring`/`aws-lc-sys` graph
  required additional Windows build tools during the Linux-hosted cross-check,
  breaching the dependency tolerance. Date/Author: 2026-06-25, implementation
  agent.
- Decision: resolve OpenSSL portability by removing only the crate's direct
  vendored `openssl-sys` dependency and keeping `postgresql_embedded` on native
  TLS. Rationale: without the direct vendored dependency, Windows and macOS use
  platform TLS and no longer build OpenSSL in the cross-target checks; Linux
  keeps the native OpenSSL path covered by the existing Linux gates. This
  avoids adding runtime crates or runner build tools while satisfying all three
  `cargo check --target ... --all-targets` checks with `-D warnings`.
  Date/Author: 2026-06-25, implementation agent.
- Decision: implement the Windows shared-cluster reaper without adding a direct
  `windows-sys` dependency. The hook uses the existing `libc::atexit`
  registration, `taskkill /PID <pid> /T` followed by
  `taskkill /PID <pid> /T /F` on timeout, and a private `kernel32` FFI wrapper
  only for liveness probing. Rationale: this preserves the public API, honours
  the no-new-runtime dependency tolerance, and kills the PostgreSQL process
  tree rather than only the postmaster process. Date/Author: 2026-06-25,
  implementation agent.
- Decision: replace the Windows reaper's `taskkill` shell-out with direct
  Win32 process-tree termination, still without adding `windows-sys`. The hook
  enumerates descendants with a Toolhelp process snapshot, terminates children
  before the postmaster with `TerminateProcess`, and waits briefly on each
  handle. Rationale: hosted Windows CI proved `taskkill` was not dependable
  inside the atexit path, while adding a Windows crate would breach the plan's
  dependency tolerance. Date/Author: 2026-06-25, implementation agent.
- Decision: continue past the original two-attempt Windows cleanup tolerance
  after explicit user approval on 2026-06-25. Try up to four additional
  approaches, recording each in this ExecPlan and validating with the hosted
  Windows orphan-detection test. Rationale: the original tolerance correctly
  forced escalation before a third cleanup strategy; the user has now supplied
  the required direction to keep iterating. Date/Author: 2026-06-25,
  implementation agent.
- Decision: make the third Windows cleanup approach a Job Object failsafe
  rather than another command executed from the atexit callback. Rationale:
  assigning the postmaster tree to a kill-on-close job gives the operating
  system an exit-time handle to close even if the callback does not run, cannot
  acquire state, or cannot finish process termination. Date/Author: 2026-06-25,
  implementation agent.
- Decision: require Windows cleanup to verify the live postmaster identity
  before Job Object assignment or process termination. Rationale: a PID-only
  cleanup path can target an unrelated process after PID reuse; comparing the
  expected PostgreSQL start timestamp from `postmaster.pid` with the live
  process creation time keeps cleanup conservative without changing public
  APIs. Date/Author: 2026-06-25, implementation agent.
- Decision: serialize the shutdown-hook integration tests with the existing
  scenario guard instead of treating "data directory exists but is not empty"
  as a soft skip. Rationale: that error is expected only under concurrent use
  of the shared test cluster directory; serializing preserves the regression
  value of the orphan-detection test and keeps failures meaningful on CI
  runners. Date/Author: 2026-06-25, implementation agent.
- Decision: implement the non-Unix scenario process lock as a lock directory
  with a bounded wait instead of adding a test dependency or using another FFI
  boundary. Rationale: directory creation is atomic on the supported
  filesystems and works on Windows runners, while the existing Unix `flock`
  path remains unchanged. Date/Author: 2026-06-25, implementation agent.
- Decision: make the new macOS and Windows CI test legs required by the
  workflow rather than advisory. Rationale: the purpose of this change is to
  prove unprivileged `TestCluster` execution and orphan cleanup on those
  platforms; advisory legs would not protect the promised support boundary.
  Initial cost budget is approximately `42` runner-minutes per pull request:
  Linux unprivileged about `14`, Linux root about `8`, macOS about `10`, and
  Windows about `10`, with a parallel wall-clock target under `15` minutes.
  Date/Author: 2026-06-25, implementation agent.
- Decision: pin the cross-platform CI tests to `PG_VERSION_REQ="=17.4.0"` and
  cache `PG_BINARY_CACHE_DIR` by PostgreSQL version, runner OS, and runner
  architecture. Rationale: the plan requires mandatory theseus download caching
  keyed by pinned PostgreSQL version; setting the version in CI avoids a broad
  cache key that silently changes when upstream defaults move. Date/Author:
  2026-06-25, implementation agent.
- Decision: gate root-specific privilege tests to the same root-capable Unix
  target set as the public `nobody_uid` and directory ownership helpers.
  Rationale: macOS support is the unprivileged in-process path; compiling
  Linux/BSD owner-changing tests there contradicts the existing public API
  boundary and fails before runtime skips can apply. Date/Author: 2026-06-25,
  implementation agent.
- Decision: gate the non-UTF-8 worker PATH-entry fixture off macOS while
  leaving it active on Unix targets that can create the raw-byte path
  component. Rationale: the test is meant to validate bootstrap handling of a
  non-UTF-8 `PATH` entry after the filesystem object exists; on macOS the
  filesystem rejects the fixture path first, so the failure does not test
  product behaviour. Date/Author: 2026-06-25, implementation agent.
- Decision: gate `staged_worker_is_world_executable_and_in_temp_dir` to the
  same root-capable Unix target set as the public `nobody_uid` helpers and
  compare cache directory expectations as `Utf8PathBuf` values. Rationale:
  macOS support is the unprivileged in-process path, so root/nobody traversal
  checks should not run there; Windows path tests should assert the semantic
  path chosen by `resolve_cache_dir`, not the separator spelling of a POSIX
  string. Date/Author: 2026-06-25, implementation agent.

## Outcomes & retrospective

To be completed at milestone boundaries and at completion. Compare the result
against the Purpose: the same `TestCluster` tests pass on Linux, macOS, and
Windows unprivileged, leave no orphaned postmaster, and `cargo binstall`
installs and runs the CLI on all three.

## Context and orientation

This section assumes no prior knowledge of the repository.

`pg-embed-setup-unpriv` is a Rust crate (edition 2024, `rust-version = 1.85`,
package version `0.5.1`) that provides zero-configuration PostgreSQL test
fixtures. Its central type, `TestCluster`, starts an embedded PostgreSQL server
on construction and stops it on drop. The embedded server comes from the
`postgresql_embedded` crate (version 0.20.2, `tokio` feature), which downloads
prebuilt PostgreSQL binaries at runtime from the theseus release host and
caches them under the user's home directory. The download requires outbound
network and benefits from `GITHUB_TOKEN` to avoid `api.github.com` rate limits.

The crate has two privilege modes. On Linux it can run as root, delegating
filesystem work to a `pg_worker` subprocess that drops to the `nobody` account
using `nix`/`libc`. When unprivileged, it runs entirely in-process. macOS and
Windows only ever use the in-process, unprivileged path; the privilege-drop
machinery is intentionally gated to Linux and the BSDs.

Key files and their current platform posture (verified against HEAD):

- `src/lib.rs` — crate root. Lines 16-231 gate the `privileges` module and its
  re-exports to Linux/BSD. `mod fs;` (line 14) is unconditional.
- `src/fs.rs` — capability-based filesystem helpers. Unconditionally imports
  `cap_std::fs::PermissionsExt` (line 7) and calls `Permissions::from_mode`
  (line 159), both Unix-only in cap-std. Reached on the unprivileged path. This
  is the genuine unconditional Windows compile blocker.
- `src/cluster/mod.rs` — `#[cfg(unix)] mod shutdown_hook;` (lines 61-62);
  `process_is_running`/`read_postmaster_pid` re-exports gated (lines 63-67).
  Already correct for Windows compilation.
- `src/cluster/handle.rs` — `register_shutdown_on_exit_impl` delegates to the
  cross-platform shutdown hook path. Unix uses the existing POSIX `atexit`
  /signal reaper; Windows registers a Job Object failsafe plus direct
  process-tree termination guarded by PostgreSQL process identity checks.
- `src/cluster/shutdown_hook/` — POSIX and Windows cleanup implementations live
  behind a platform facade. Public helper signatures remain unchanged; Windows
  parses `postmaster.pid` into PID plus start timestamp to avoid PID-reuse
  mistakes.
- `src/test_support/shared_singleton.rs` — provides
  `shared_cluster()`/`shared_cluster_handle()`, which call
  `register_shutdown_on_exit` and `std::mem::forget` the guard. This is the
  path validated by the cross-platform orphan-detection test.
- `Cargo.toml` — `nix` is Unix-only, the dead direct `xdg` dependency and direct
  vendored `openssl-sys` dependency are removed, and
  `[package.metadata.binstall]` retains one `.tgz` layout for every supported
  target.
- `tests/settings.rs` — root-specific assertions are gated to root-capable Unix
  targets, while the remaining settings tests compile everywhere.
- `.github/workflows/ci.yml` — Linux root/unprivileged jobs remain the
  authoritative lint/test/coverage path; macOS and Windows test-only jobs run
  the unprivileged surface; a `binstall` job builds a local archive per OS,
  performs a real install from local HTTPS, checks both binaries, and runs the
  installed CLI.
- `.github/workflows/release.yml` — on `v*`, `build-assets` publishes Linux
  x86-64, Linux arm64, Windows x86-64, macOS arm64, and macOS Intel archives by
  calling `scripts/release_archive.py`, then uploads `dist/*.tgz`.
- `Makefile` — `release-archive` validates `TARGET`/`VERSION` and delegates to
  `scripts/release_archive.py` for the cross-platform archive layout.
- `.config/nextest.toml` — `global-timeout = "10m"`; a `serial` test group; a
  30s cap on the `settings` binary.

External facts established during research:

- cargo-binstall template variables resolve per target: `{ binary-ext }` is
  `.exe` on Windows, empty elsewhere; `{ archive-suffix }` derives from
  `pkg-fmt`. Per-target overrides under
  `[package.metadata.binstall.overrides.<target-or-cfg>]` may override
  `pkg-url`, `pkg-fmt`, and `bin-dir`; exact target names beat `cfg(...)`
  expressions, and multiple matching `cfg` expressions evaluate in declaration
  order.
- `cargo binstall` flags for CI: `--dry-run` (resolve+fetch, no install — does
  not extract), `--manifest-path`, `--targets <triple>`,
  `--strategies crate-meta-data` (use only package metadata so a fallback
  cannot mask a broken URL), `--install-path <dir>`, `--no-confirm`,
  `--github-token`.
- Runners: `windows-latest` = `windows-2025` (x64); `macos-latest` = `macos-15`
  on Apple Silicon (arm64); current standard Intel macOS labels include
  `macos-15-intel` and `macos-26-intel`. Windows-on-ARM runners exist but have
  no upstream PostgreSQL binaries.

## Plan of work

Six milestones with explicit go/no-go validation at each boundary. Do not start
a milestone before the previous validates green on Linux.

### Milestone 0 — Prototype: observe the real compile blockers (gating)

This is the de-risking gate, and it must run *first and for real*. It validates
`cfg`-resolution only; it does not validate linking (vendored OpenSSL needing
Perl/NASM, `pq-sys` libpq) or any runtime behaviour. Do not present a green
Milestone 0 as de-risking Milestones 2-3.

```bash
rustup target add x86_64-pc-windows-msvc x86_64-apple-darwin aarch64-apple-darwin
cargo check --target x86_64-apple-darwin   --all-targets 2>&1 | tee /tmp/check-darwin.out
cargo check --target aarch64-apple-darwin  --all-targets 2>&1 | tee /tmp/check-darwin-arm.out
cargo check --target x86_64-pc-windows-msvc --all-targets 2>&1 | tee /tmp/check-windows.out
```

Record every distinct error in `Surprises & discoveries` and rewrite the Risk
register to match. Confirm or refute: `fs.rs` mode blocker, `nix` in the
Windows graph, `tests/settings.rs` `nix` import, the dead `xdg` crate, and
whether `openssl-sys` is even pulled in. Go/no-go: a written, observed blocker
list. If it exceeds the Milestone 1 scope tolerance, stop and escalate.

### Milestone 1 — Compile and cleanup correctly on Windows and macOS

Scope is whatever Milestone 0 observed; the tasks below are the expected set.

1. Reorganise dependencies in `Cargo.toml`: move `nix` to
   `[target.'cfg(unix)'.dependencies]`; remove the dead direct `xdg`
   dependency; remove the direct vendored `openssl-sys` dependency; keep
   `postgresql_embedded` on native TLS because the `rustls` attempt required
   extra Windows build tools during cross-checking. Keep caret requirements.

2. Gate `src/fs.rs`: apply `Permissions::from_mode`/`PermissionsExt` only under
   `#[cfg(unix)]`; provide a Windows no-op (or access-control equivalent) that
   preserves the function signatures. Add a code comment and a users'-guide
   note that POSIX `0o700` privacy is not enforced on Windows.

3. Audit the `tests/` tree for top-level Unix-only imports. Gate
   `tests/settings.rs:7` (`use nix::unistd::geteuid;`) and its dependent bodies
   with `#[cfg(unix)]`; fix any others found. Re-run the Milestone 0
   cross-checks until clean.

4. Resolve Windows shared-cluster cleanup (the behavioural task). Add an
   orphan-detection integration test (host-gated) that takes the
   `shared_singleton` path, `mem::forget`s the guard, exits the process, and
   asserts from a parent harness that the postmaster PID is no longer alive.
   - Red: the test fails on Windows (no-op hook leaks the postmaster).
   - Green: implement a real Windows reaper — register a process-exit cleanup
     using Win32 Job Objects (kill-on-close) or `OpenProcess`+`TerminateProcess`
     reading the postmaster PID — keeping `register_shutdown_on_exit`'s signature
     identical across platforms. If, instead, Milestone 0/analysis proves no
     supported cross-platform test path forgets a guard, encode that as the test
     (assert Drop reaps it) and document the limitation rather than adding a
     reaper. Record which branch was taken in the Decision log.
   - Note: macOS is Unix, so the existing POSIX hook already compiles and runs
     there; this task is Windows-specific.

5. Confirm both declared binaries (`pg_embedded_setup_unpriv` and `pg_worker`)
   compile on all platforms; `pg_worker`'s non-Unix `main` stub remains.

Go/no-go: the three cross-checks pass; the Linux gateway is unchanged and
green; and one *real* Windows link-and-run is exercised before proceeding — a
throwaway `workflow_dispatch` job on `windows-latest` that builds the crate and
starts a single `TestCluster` — so link/runtime surprises (OpenSSL, `pq-sys`,
theseus download, TCP sockets, the new reaper) land while the change set is
still small. Commit per atomic change.

### Milestone 2 — Test-only macOS and Windows CI matrix

Extend `.github/workflows/ci.yml`. Keep the rich Linux job authoritative (it
owns format, Markdown lint, Clippy, coverage, CodeScene) and add a lean
cross-platform job. Prefer the `rust-toy-app.yml` matrix shape from
`shared-actions` as the template.

```yaml
strategy:
  fail-fast: false
  matrix:
    include:
      - os: macos-latest      # aarch64-apple-darwin (Apple Silicon)
      - os: windows-latest    # x86_64-pc-windows-msvc
runs-on: ${{ matrix.os }}
```

Each leg: check out; run `leynos/shared-actions/.github/actions/setup-rust`
(bumped pin); restore a **mandatory** `actions/cache` for the theseus
PostgreSQL download keyed on the pinned PostgreSQL version + OS + arch; run the
unprivileged test suite plus the orphan-detection test. Pass `GITHUB_TOKEN`. Do
not run lint, format, coverage, or CodeScene upload. Add `concurrency:`
cancellation for superseded pushes to bound cost. Choose feature sets
deliberately and symmetrically:

- macOS and Windows both start with
  `--no-default-features --features cluster-unit-tests,async-api` (the
  unprivileged surface, no `diesel-support`), so neither pays the `pq-sys`
  libpq-from-source build at first. Enabling `diesel-support` (and thus the
  `test_cluster_connection` test) on each platform is a deliberate, separately
  budgeted follow-up; record whether it builds. Do not silently use
  `--all-features` on macOS — that pulls `pq-sys` at ~10x billing.
- Install `cargo-nextest` the same way the Linux job does
  (`cargo binstall -y cargo-nextest@<pinned>`), which works on Windows and
  macOS.

Record an expected per-PR CI-minute budget (Linux ×2 privilege + macOS +
Windows) and a wall-clock target. Confirm the `.config/nextest.toml` 10-minute
global timeout still holds once the (cached) PostgreSQL start time is included;
warm the cache in a prior step if needed. Decide and document whether the new
legs are required or advisory.

Go/no-go: macOS and Windows legs (including orphan detection) are green on a
pull request; the Linux job is unchanged and green; the cost budget is recorded.

### Milestone 3 — binstall packaging for macOS and Windows

1. Keep the existing `Cargo.toml` `[package.metadata.binstall]` block:
   `pkg-url`, `bin-dir`, and `pkg-fmt = "tgz"`. Do not add a Windows `zip`
   override. The `bin-dir` template already appends `{ binary-ext }`, yielding
   `pg_worker.exe` on Windows while the archive itself remains tar.gz.

2. Produce the archives with `scripts/release_archive.py`. The script builds
   the two production binaries, stages target-specific executable names, and
   writes a `.tgz` whose root directory matches the `bin-dir` template.
   `make release-archive` delegates to the script for local development, and
   GitHub workflows call the script directly so Windows does not depend on GNU
   Make being available.

3. Extend the `build-assets` matrix in `release.yml` to add
   `x86_64-pc-windows-msvc` (`windows-latest`), `aarch64-apple-darwin`
   (`macos-latest`), and `x86_64-apple-darwin` (`macos-15-intel`). Upload
   archives whose names exactly match the `pkg-url` template per target.

Go/no-go: each OS produces a correctly named archive whose internal layout
matches `bin-dir` (proven by Milestone 4's real install, not just by
inspection).

### Milestone 4 — Validate binstall by real install-and-run

1. Pull-request-time real install. On each of `ubuntu-latest`, `macos-latest`,
   and `windows-latest`, build the archive (via the same code path as the
   release job), then run a real, non-dry-run install into a scratch directory
   and execute the installed binary:

   ```bash
   cargo binstall --manifest-path Cargo.toml --targets <host-triple> \
     --strategies crate-meta-data --install-path "$tmp" --no-confirm \
     pg-embed-setup-unpriv
   "$tmp/pg_embedded_setup_unpriv" --version   # .exe on Windows
   ```

   This proves extraction and `bin-dir` placement on the real OS — the failure
   `--dry-run` cannot catch. The implemented mechanism points cargo-binstall at
   the locally built archive by serving `dist/` over local HTTPS with a
   throwaway CA and passing that CA through `--root-certificates`; plain
   `http://` and `file://` package URLs are rejected by this cargo-binstall
   build.

2. Release-time breadth audit. After `build-assets`, on each OS, loop
   `cargo binstall --manifest-path Cargo.toml --targets <triple> --strategies crate-meta-data --github-token "$GITHUB_TOKEN" --no-confirm --dry-run pg-embed-setup-unpriv`
   over all supported triples to confirm every published URL resolves, plus
   one real install-and-run of the host target's asset.

Go/no-go: the real install-and-run passes on all three OSes at PR time, and the
release audit passes against the real assets.

### Milestone 5 — Documentation and finalisation

Update the README install section to a platform/target matrix stating macOS
(arm64 and x86-64) and Windows (x86-64) support, the `cargo binstall` command,
the Windows-on-ARM limitation, and the Windows file-mode-privacy caveat. Update
`docs/users-guide.md` and `docs/roadmap.md` (appendix and item 3.3.2 — now
validated in CI), and update
`docs/zero-config-raii-postgres-test-fixture-design.md` with the Windows
cleanup/reaper decision and the `fs.rs` mode-gating decision. Run `make fmt`,
`make markdownlint`, and `make nixie`, then the full gateway. Mark the plan
COMPLETE in `Outcomes & retrospective`.

## Concrete steps

Run from the repository root. Capture long outputs with `tee` to
`/tmp/<action>-<branch>.out` per the project command guidance.

1. Milestone 0 cross-checks (commands in the Milestone 0 section).
2. After each code change, on Linux:

   ```bash
   make check-fmt 2>&1 | tee /tmp/fmt-windows-mac-support-validation.out
   make lint      2>&1 | tee /tmp/lint-windows-mac-support-validation.out
   make test      2>&1 | tee /tmp/test-windows-mac-support-validation.out
   ```

3. After documentation changes:

   ```bash
   make fmt
   make markdownlint 2>&1 | tee /tmp/mdlint-windows-mac-support-validation.out
   make nixie        2>&1 | tee /tmp/nixie-windows-mac-support-validation.out
   ```

4. Record here, as observed: the Milestone 0 blocker list; the throwaway
   Windows link-and-run run URL; the green macOS/Windows/Linux CI URLs; the
   recorded CI-minute budget; and the binstall install-and-run output per OS.

This section must be updated with real transcripts and URLs as work proceeds.

## Validation and acceptance

Acceptance is behavioural and observable:

- Library portability:
  `cargo check --target x86_64-pc-windows-msvc --all-targets`,
  `--target x86_64-apple-darwin`, and `--target aarch64-apple-darwin` all
  complete without error after Milestone 1, with the dead `xdg` dependency
  removed and `nix` Unix-gated.
- Cleanup correctness: the orphan-detection test fails on Windows before the
  fix (Red — leaked postmaster) and passes after (Green — no orphan), and runs
  in the cross-platform matrix.
- CI matrix: on a pull request, the macOS (`macos-latest`) and Windows
  (`windows-latest`) legs run `cargo nextest` and the orphan-detection test to
  green, exercising the unprivileged in-process `TestCluster` path; they run
  tests only. The Linux job remains green and unchanged in scope. A recorded CI
  budget exists.
- binstall (real): on each OS, `cargo binstall --install-path <tmp>` installs
  the CLI and the installed binary runs `--version` successfully. The
  release-time dry-run audit resolves all five supported triples.
- Scope honesty: the Windows `diesel-support`/`pq-sys` path and Intel-macOS
  execution are explicitly documented as validated-later or
  resolve-only-not-executed, not silently implied as covered.
- Documentation: README, users' guide, and roadmap describe supported platforms,
  the Windows-on-ARM limitation, and the file-mode-privacy caveat;
  `make markdownlint` and `make nixie` pass.

Quality criteria ("done"):

- Tests: full `make test` passes on Linux; macOS and Windows nextest legs plus
  orphan detection pass in CI.
- Lint/typecheck: `make lint` and `make check-fmt` pass on Linux with the
  existing `-D warnings` ceiling; no new `allow` attributes without a scoped
  reason.
- Packaging: `cargo binstall` installs and runs the CLI on all three OSes.

Quality method: the CI workflows above are the automated check; the
orphan-detection Red→Green test and the real binstall install-and-run are the
behavioural checks for the two genuinely new behaviours.

## Idempotence and recovery

Adding `rustup` targets, editing workflows, editing `Cargo.toml` metadata,
removing a dead dependency, and adding tests are all idempotent and
re-runnable. The release dry-run audit installs nothing; the real install
targets a scratch directory. If a CI leg fails, re-running the workflow is
safe. If archive naming is wrong, fix the metadata/packaging and re-run; no
production state is mutated by validation. Keep the working tree clean: remove
`/tmp` logs and any throwaway tags or `workflow_dispatch` artefacts after use.

## Artifacts and notes

Capture as the work produces them: the Milestone 0 cross-compile transcripts;
the first green Windows and macOS nextest runs; the orphan-detection test
output; the resolved `binstall` URLs and the real install-and-run output per
target; and the recorded CI-minute budget. The `shared-actions`
`rust-toy-app.yml` matrix is the canonical cross-OS reference.

## Interfaces and dependencies

- `Cargo.toml` dependency changes: move `nix` to
  `[target.'cfg(unix)'.dependencies] nix = { version = "0.30.1", default-features = false, features = ["user", "fs"] }`;
  remove the dead direct `xdg = "3"`; remove the direct vendored `openssl-sys`
  dependency; keep
  `postgresql_embedded = { version = "0.20.2", features = ["tokio"] }` on its
  default native-TLS backend. No new runtime crates are expected (a Win32
  reaper can use `std`/`windows-sys` only if Milestone 1 shows it necessary —
  adding `windows-sys` would be a new dependency and triggers the dependency
  tolerance, so escalate before adding it).
- `src/fs.rs`: keep `set_permissions`/`ensure_dir_exists` signatures unchanged;
  apply mode bits only under `#[cfg(unix)]`.
- `src/cluster/shutdown_hook.rs` and `register_shutdown_on_exit`: no
  cross-platform signature change. The module is already `cfg(unix)`-gated and
  its `libc::pid_t` parameters never reach Windows. Do not introduce a `pid_t`→
  `i32` rename (the earlier draft's suggestion); it is unnecessary and would
  churn the Unix-only re-exports.
- New non-Unix cleanup: `register_shutdown_on_exit` must, on Windows, register a
  real process-exit reaper (preferred: a Job Object created at cluster start so
  the postmaster is killed on handle close), preserving the existing
  `fn register_shutdown_on_exit(&self) -> BootstrapResult<()>` signature.
- `Cargo.toml` `[package.metadata.binstall]`: retain `pkg-url`, `bin-dir`,
  and `pkg-fmt = "tgz"` for every supported target. Do not add a Windows
  override unless a future cargo-binstall version requires it.
- New build targets to publish: `x86_64-pc-windows-msvc`, `x86_64-apple-darwin`,
  `aarch64-apple-darwin`, alongside the existing two Linux targets.
- GitHub Actions: reuse `leynos/shared-actions` `setup-rust`, pinned to the
  bumped SHA; add `actions/cache` for the theseus download. Never run
  `upload-codescene-coverage` on Windows/macOS.
- `scripts/release_archive.py`: build both production binaries for the requested
  target and package them in the cargo-binstall `.tgz` layout. Keep tests in
  `scripts/tests/test_release_archive.py`.

## Revision note

Revision 2 (2026-06-25), after a Logisphere community-of-experts design review.
What changed and why:

- Corrected a load-bearing factual error: `src/cluster/shutdown_hook.rs` is
  already `#[cfg(unix)]`-gated (`mod.rs:61`) with a `#[cfg(not(unix))]` no-op
  (`handle.rs:325-330`), so it is not a Windows compile blocker. Removed the
  "port shutdown_hook / TerminateProcess / Red-Green shutdown-hook compile" and
  the `pid_t`→`i32` workstreams.
- Replaced the wrong primary blocker with the real ones: `src/fs.rs`
  (cap-std `PermissionsExt`/`from_mode`), the unconditional `nix` dependency,
  the unconditional `use nix::unistd::geteuid;` in `tests/settings.rs`, and the
  apparently-dead Unix-only `xdg` crate. Added a `tests/` audit.
- Added the genuine new risk the original missed: the non-Unix no-op shutdown
  hook leaves `mem::forget` shared clusters unreaped on Windows (orphaned
  postmaster). Reframed the Windows work as behavioural cleanup proven by an
  orphan-detection Red→Green test.
- Strengthened binstall validation from `--dry-run` (which does not extract) to
  a real install-and-run per OS; flagged the three coupled packaging edit sites.
- Made theseus download caching mandatory (not "where practical"), added a CI
  cost budget and concurrency cancellation, and made the macOS/Windows feature
  sets symmetric so macOS does not silently build `pq-sys` via `--all-features`.
- Flipped the packaging default to reuse
  `shared-actions/stage-release-artefacts` and the `rust-toy-app.yml` matrix;
  demoted the bespoke Python self-test to a last resort (still df12-compliant
  if needed). This revision note is historical; Milestone 3 later superseded
  the staging-action path after implementation inspection found it packaged
  only one binary.
- Reframed Milestone 0 as `cfg`-resolution-only de-risking and pulled one real
  Windows link-and-run into the Milestone 1 exit gate.

How it affects remaining work: Milestone 1's scope shifts from a non-existent
compile port to `fs.rs`/dependency gating plus a Windows reaper; the tolerances
were re-baselined accordingly. No implementation has begun; Status is DRAFT
pending review.

Revision 3 (2026-06-25), at implementation start. What changed and why:

- Marked the ExecPlan `IN PROGRESS` after explicit user approval to proceed.
- Added a timestamped Milestone 0 progress entry and decision-log note so a
  future reader can distinguish approved implementation work from the earlier
  planning-only pull request.

How it affects remaining work: Milestone 0 must still run first and re-ground
the risk list from observed cross-target compiler output before Milestone 1
code changes begin.
