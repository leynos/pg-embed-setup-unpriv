# ADR 004: enhanced lint and formatting requirements

## Status

Accepted on 2026-07-03. The repository imports the shared Rust formatting and
Clippy policy, pins a dated nightly toolchain for unstable rustfmt options, and
adds `interrogate` as a Python docstring-coverage tier in the lint gate.

## Date

2026-07-03.

## Context and problem statement

The project already requires Rust formatting, Rust documentation checks,
Clippy, and tests before changes are accepted. The previous policy left two
gaps:

- Rust formatting and Clippy settings could drift from the shared agent
  template used across Leynos Rust projects.
- Python helper files could pass linting even when documentable functions,
  fixtures, or test utilities lacked docstrings.

The shared template rustfmt configuration uses unstable rustfmt options, so
adopting it requires a nightly toolchain. Using the floating `nightly` channel
would make formatting results change as upstream nightly builds change.

## Decision drivers

- Keep Rust formatting and Clippy policy aligned with the shared Leynos Rust
  agent template.
- Make formatting reproducible by pinning the nightly toolchain to a dated
  release.
- Enforce complete Python docstring coverage through an objective gate.
- Keep the new checks inside the existing `make lint` and CI workflow rather
  than introducing a parallel contributor process.

## Options considered

### Option A: keep the existing stable formatter and lint policy

Continue using the previous stable rustfmt behaviour and omit Python docstring
coverage checks.

This avoids toolchain churn, but it leaves the repository out of sync with the
shared Rust template and keeps Python documentation coverage subjective.

### Option B: import the template policy and pin nightly rustfmt

Add the template `rustfmt.toml` and Clippy policy, pin `rust-toolchain.toml` to
`nightly-2026-04-25`, and run `interrogate --fail-under 100 .` as the first
`make lint` tier.

This provides reproducible formatting, keeps Rust lint policy aligned with the
template, and makes Python docstring coverage complete and objective. The cost
is that contributors must use the pinned nightly toolchain for formatting.

### Option C: use a floating nightly channel

Adopt the template configuration but set `rust-toolchain.toml` to `nightly`.

This keeps the configuration small, but rustfmt and Clippy results may drift as
nightly changes. That drift makes CI failures harder to reproduce and upgrades
less intentional.

## Decision outcome

Choose Option B.

The repository pins `rust-toolchain.toml` to `nightly-2026-04-25` so unstable
rustfmt features from the imported template remain reproducible. `make lint`
runs these tiers in order:

1. `interrogate --fail-under 100 .` for Python docstring coverage.
2. `cargo doc --workspace --no-deps` with warnings denied.
3. `cargo clippy --all-targets --all-features -- -D warnings`.

CI installs `interrogate==1.7.0` as a uv tool before the lint step, keeping the
docstring-coverage gate pinned with the workflow.

## Consequences

### Positive

- Rust formatting and Clippy policy match the shared Leynos template.
- Nightly rustfmt output is reproducible because the channel is date-pinned.
- Python docstring coverage is enforced at 100% by a dedicated tool.

### Negative

- Contributors need the pinned nightly toolchain for formatting checks.
- New or changed Python helpers must document every documentable node.
- Future rustfmt or Clippy template upgrades require an intentional toolchain
  update rather than an implicit nightly drift.
