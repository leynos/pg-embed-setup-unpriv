"""Contract for the dedicated Loom workflow.

Loom model checking has its own lane: a pull request runs the small profile,
the daily schedule runs the larger one, and a manual dispatch chooses. These
tests read `.github/workflows/loom.yml` as GitHub does and hold every trigger,
cache, step and exclusion in place, so a change that quietly folds Loom back
into the build lane or drops the report fails here.
"""

from __future__ import annotations

import tomllib
import typing as typ
from pathlib import Path

import pytest

from workflow_reader import Workflow, load_workflows

REPO_ROOT = Path(__file__).resolve().parents[2]
LOOM_PATH = REPO_ROOT / ".github" / "workflows" / "loom.yml"
CI_PATH = REPO_ROOT / ".github" / "workflows" / "ci.yml"
MANIFEST = REPO_ROOT / ".config" / "loom-models.toml"

CHECKOUT = "actions/checkout@"
CACHE = "actions/cache@"
SETUP_RUST = "leynos/shared-actions/.github/actions/setup-rust@"
SETUP_UV = "astral-sh/setup-uv@"
UPLOAD = "actions/upload-artifact@"


def loom() -> Workflow:
    """Return the parsed Loom workflow."""
    return Workflow.parse(LOOM_PATH.name, LOOM_PATH.read_text(encoding="utf-8"))


def steps_using(workflow: Workflow, prefix: str) -> list[dict[str, typ.Any]]:
    """Return the steps that use an action whose reference starts with `prefix`."""
    return [
        step
        for _, step in workflow.steps()
        if str(step.get("uses", "")).startswith(prefix)
    ]


def step_named(workflow: Workflow, name: str) -> dict[str, typ.Any]:
    """Return the step with `name`."""
    for _, step in workflow.steps():
        if step.get("name") == name:
            return step
    raise AssertionError(f"no step named {name!r}")


def test_the_workflow_answers_pull_requests_the_schedule_and_dispatch() -> None:
    """All three triggers are present, with the documented schedule and input."""
    triggers = loom().triggers

    assert triggers["pull_request"]["types"] == ["opened", "synchronize", "reopened"], (
        "a pull request must run the lane on open and on every push"
    )
    assert triggers["schedule"] == [{"cron": "45 17 * * *"}], (
        "the daily run must keep its documented schedule"
    )
    profile = triggers["workflow_dispatch"]["inputs"]["profile"]
    assert profile["type"] == "choice", "a manual dispatch must choose a profile"
    assert profile["options"] == ["pr", "scheduled"], (
        "a manual dispatch must offer exactly the two profiles"
    )
    assert profile["default"] == "scheduled", (
        "a manual dispatch must default to the scheduled profile"
    )


def test_the_workflow_reads_contents_only() -> None:
    """The lane needs no write permission."""
    assert loom().document["permissions"] == {"contents": "read"}, (
        "the lane must not request write access"
    )


def test_concurrency_cancels_only_pull_requests() -> None:
    """A newer pull request supersedes its own run; a scheduled run is kept."""
    concurrency = loom().document["concurrency"]

    assert concurrency["group"] == (
        "${{ github.workflow }}-${{ github.event_name }}-${{ github.ref }}"
    ), "the group must separate events and refs so a schedule is never shared"
    assert concurrency["cancel-in-progress"] == (
        "${{ github.event_name == 'pull_request' }}"
    ), "only a pull request may cancel its own superseded run"


def test_one_literal_hosted_job_with_a_ceiling_above_its_step() -> None:
    """The job needs no matrix or placement expression, and outlives its step."""
    jobs = loom().jobs()

    assert len(jobs) == 1, "the lane must stay a single job"
    _, job = jobs[0]
    assert "needs" not in job, "the lane must not depend on another job"
    assert job["runs-on"] == "ubuntu-latest", (
        "the lane must prefer the hosted default host"
    )
    run = step_named(loom(), "Run Loom models")
    assert job["timeout-minutes"] > run["timeout-minutes"], (
        "the job must outlive its run step so the upload still happens"
    )


@pytest.mark.parametrize("profile", ["pr", "scheduled"])
def test_each_run_budget_sits_below_the_run_step_ceiling(profile: str) -> None:
    """The runner must write its summary before the step is cancelled."""
    document = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    ceiling = step_named(loom(), "Run Loom models")["timeout-minutes"] * 60

    budget = document["profiles"][profile]["run_budget_seconds"]

    assert budget < ceiling, (
        f"the {profile} run budget must sit below the step ceiling, "
        "or the summary would never be written"
    )


def test_the_steps_run_in_the_documented_order() -> None:
    """Checkout, caches, toolchains, then the run."""
    steps = [step for _, step in loom().steps()]

    def position(predicate: typ.Callable[[dict[str, typ.Any]], bool]) -> int:
        """Return the index of the first step matching `predicate`."""
        return next(index for index, step in enumerate(steps) if predicate(step))

    positions = [
        position(lambda step: str(step.get("uses", "")).startswith(CHECKOUT)),
        position(lambda step: "target" in str(step.get("with", {}).get("path", ""))),
        position(lambda step: "rustup" in str(step.get("with", {}).get("path", ""))),
        position(lambda step: str(step.get("uses", "")).startswith(SETUP_RUST)),
        position(lambda step: str(step.get("uses", "")).startswith(SETUP_UV)),
        position(lambda step: step.get("name") == "Run Loom models"),
    ]
    assert positions == sorted(positions), (
        "checkout, caches, toolchains and the run must keep their order"
    )


def test_checkout_keeps_no_credentials() -> None:
    """The lane never pushes, so the token is not persisted."""
    checkout = steps_using(loom(), CHECKOUT)

    assert len(checkout) == 1, "the lane must check out exactly once"
    assert checkout[0]["with"]["persist-credentials"] is False, (
        "the checkout must not persist the token"
    )


def test_the_cargo_cache_covers_registry_git_and_target() -> None:
    """The compile cache is keyed on the lockfile, not the runtime profile."""
    caches = steps_using(loom(), CACHE)
    cargo = next(step for step in caches if "target" in step["with"]["path"])

    assert "~/.cargo/registry" in cargo["with"]["path"], (
        "the cache must cover the registry"
    )
    assert "~/.cargo/git" in cargo["with"]["path"], "the cache must cover Git sources"
    assert "hashFiles('Cargo.lock'" in cargo["with"]["key"], (
        "the cache key must follow the lockfile"
    )
    assert "LOOM_PROFILE" not in cargo["with"]["key"], (
        "both profiles must share one compile cache"
    )


def test_the_rustup_cache_is_separate_and_keyed_on_the_toolchain() -> None:
    """A toolchain change invalidates only the toolchain cache."""
    caches = steps_using(loom(), CACHE)
    rustup = next(step for step in caches if "rustup" in step["with"]["path"])

    assert rustup["with"]["path"] == "~/.rustup/toolchains", (
        "the toolchain cache must not share the cargo cache's path"
    )
    assert "hashFiles('rust-toolchain.toml')" in rustup["with"]["key"], (
        "the toolchain cache key must follow the toolchain file"
    )


def test_setup_rust_installs_mold_and_setup_uv_follows() -> None:
    """The Linux linker is installed, and uv is available for the runner."""
    rust = steps_using(loom(), SETUP_RUST)

    assert len(rust) == 1, "the lane must set up Rust exactly once"
    assert rust[0]["with"]["install-mold"] == "true", (
        "the Linux link step must install mold"
    )
    assert len(steps_using(loom(), SETUP_UV)) == 1, (
        "the lane must set up uv exactly once for the runner"
    )


def test_the_run_step_selects_the_profile_and_clears_the_wrapper() -> None:
    """The profile reaches the runner, and no local admission wrapper is used."""
    run = step_named(loom(), "Run Loom models")

    assert run["env"]["RUSTC_WRAPPER"] == "", (
        "the hosted runner must not use a local admission wrapper"
    )
    assert run["run"] == 'make test-loom LOOM_PROFILE="$LOOM_PROFILE"', (
        "the step must run the Makefile target with the mapped profile"
    )


def test_the_report_is_uploaded_even_after_a_failure() -> None:
    """A failed exploration still leaves its logs and summary."""
    uploads = steps_using(loom(), UPLOAD)

    assert len(uploads) == 1, "the lane must upload its report exactly once"
    assert uploads[0]["if"] == "always()", (
        "a failed run must still upload its report"
    )
    assert uploads[0]["with"]["path"] == "target/loom-report", (
        "the upload must point at the runner's report directory"
    )


@pytest.mark.parametrize(
    "excluded",
    [
        "nextest",
        "coverage",
        "postgres",
        "merman",
        "whitaker",
        "spelling",
        "markdownlint",
        "matrix",
    ],
)
def test_the_lane_carries_none_of_the_build_lane_concerns(excluded: str) -> None:
    """Loom is independent: no test runner, coverage, database or docs checks."""
    # The parsed jobs, not the file text: a comment may name a concern.
    structure = str(loom().jobs()).lower()

    assert excluded not in structure, (
        f"the Loom lane must not take on the {excluded} concern"
    )


def test_the_build_lane_no_longer_runs_loom() -> None:
    """The dedicated workflow is the only place Loom runs."""
    ci = Workflow.parse(CI_PATH.name, CI_PATH.read_text(encoding="utf-8"))

    assert all("loom" not in str(step).lower() for _, step in ci.steps()), (
        "the build lane must leave Loom to the dedicated workflow"
    )


def test_the_workflow_is_discovered_with_the_others() -> None:
    """The reader sees the file, so the other contracts judge it too."""
    paths = {workflow.path for workflow in load_workflows(REPO_ROOT)}

    assert ".github/workflows/loom.yml" in paths, (
        "the workflow reader must discover the Loom lane"
    )