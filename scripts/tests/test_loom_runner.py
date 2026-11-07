"""Behavioural tests for the Loom runner.

The runner builds the test binary once, discovers the ignored models, checks
them against the manifest, and runs each one with an exact name filter and a
deadline. These tests drive it through an injected process seam, so they assert
the exact commands, the exact environment overlays, and the classification of
every outcome without compiling anything.
"""

from __future__ import annotations

import json
import os
import sys
import time
import typing as typ
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

import loom_runner  # noqa: E402
from loom_discovery import DiscoveryError  # noqa: E402
from loom_process import log_name, run_process  # noqa: E402
from loom_runner import Completed  # noqa: E402

MODELS = (
    ("env::loom_tests::first", "env", 2, 3),
    ("cluster::lifecycle_loom_tests::second", "cluster", 2, 3),
)

#: Whole-run budgets large enough to leave the per-model ceilings in charge.
BUDGETS = {"pr": 300, "scheduled": 600}


def write_manifest(
    root: Path,
    models: typ.Sequence[tuple[str, str, int, int]] = MODELS,
    budgets: typ.Mapping[str, int] | None = None,
) -> Path:
    """Write a manifest with `models` and `budgets` and return its path."""
    chosen = {**BUDGETS, **(budgets or {})}
    entries = "".join(
        f'[[models]]\nname = "{name}"\nsuite = "{suite}"\n'
        f"spawned_threads = {spawned}\ncontext_threads = {context}\n\n"
        for name, suite, spawned, context in models
    )
    path = root / "loom-models.toml"
    path.write_text(
        "[profiles.pr]\n"
        f"model_timeout_seconds = 30\nrun_budget_seconds = {chosen['pr']}\n\n"
        "[profiles.scheduled]\n"
        f"model_timeout_seconds = 60\nrun_budget_seconds = {chosen['scheduled']}\n\n"
        + entries
    )
    return path


def marker(kind: str, name: str, profile: str = "pr") -> str:
    """Return a marker line for `name`."""
    return (
        f"{kind} profile={profile} max_threads=3 max_branches=64 "
        f"preemption_bound=3 max_duration=none max_permutations=none "
        f"checkpoint=none test={name}"
    )


def passing_output(name: str, profile: str = "pr") -> str:
    """Return the output of a model that ran to completion and passed.

    The markers interleave with libtest's progress line because the runner
    passes `--nocapture`, so the `ok` that completes the line arrives after
    the markers.
    """
    return "\n".join(
        [
            "running 1 test",
            f"test {name} ... {marker('loom-model-start', name, profile)}",
            marker("loom-model-complete", name, profile),
            "ok",
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 624 filtered out",
            "",
        ]
    )


class FakeRunner:
    """A process seam that answers the build, the listing and each model."""

    def __init__(
        self,
        models: typ.Sequence[tuple[str, str, int, int]] = MODELS,
        *,
        features: typ.Sequence[str] = ("loom-tests",),
        listing: typ.Sequence[str] | None = None,
        outputs: typ.Mapping[str, Completed] | None = None,
        build_returncode: int = 0,
        delay_seconds: float = 0.0,
    ) -> None:
        """Record the inventory, the canned answers and the model delay."""
        self.models = models
        self.features = list(features)
        self.listing = list(listing) if listing is not None else [name for name, *_ in models]
        self.outputs = dict(outputs or {})
        self.build_returncode = build_returncode
        self.delay_seconds = delay_seconds
        self.calls: list[tuple[list[str], dict[str, str], float | None]] = []

    def __call__(
        self,
        command: list[str],
        cwd: Path,
        environment: typ.Mapping[str, str],
        timeout: float | None = None,
    ) -> Completed:
        """Answer one command and record it."""
        self.calls.append((list(command), dict(environment), timeout))
        if command[0] == "cargo" and command[1] == "--version":
            return Completed(0, "cargo 1.0.0\n", "")
        if command[0] == "rustc":
            return Completed(0, "rustc 1.0.0\n", "")
        if command[0] == "cargo":
            artefact = {
                "reason": "compiler-artifact",
                "target": {
                    "name": "pg_embedded_setup_unpriv",
                    "kind": ["lib"],
                    "test": True,
                },
                "features": self.features,
                "executable": "/tmp/loom-tests",
            }
            return Completed(self.build_returncode, json.dumps(artefact) + "\n", "")
        if "--list" in command:
            return Completed(0, "".join(f"{name}: test\n" for name in self.listing), "")
        name = command[1]
        time.sleep(self.delay_seconds)
        return self.outputs.get(name, Completed(0, passing_output(name), ""))

    def model_calls(self) -> list[list[str]]:
        """Return the commands that ran a model, not the listing."""
        return [
            command
            for command, _, _ in self.calls
            if command[0] == "/tmp/loom-tests" and "--list" not in command
        ]


def run(
    tmp_path: Path,
    runner: FakeRunner,
    *,
    profile: str = "pr",
    environment: typ.Mapping[str, str] | None = None,
    budgets: typ.Mapping[str, int] | None = None,
) -> typ.Any:
    """Run the inventory in `tmp_path` and return the report."""
    (tmp_path / "Cargo.lock").write_text('[[package]]\nname = "loom"\nversion = "0.7.2"\n')
    return loom_runner.run_loom(
        tmp_path,
        profile,
        write_manifest(tmp_path, budgets=budgets),
        tmp_path / "report",
        "cargo",
        runner,
        {"GITHUB_SHA": "abc123", **(environment or {})},
    )


def test_runs_every_model_in_manifest_order_with_exact_arguments(tmp_path: Path) -> None:
    """Each model runs alone, by exact name, with one test thread."""
    runner = FakeRunner()

    run(tmp_path, runner)

    assert runner.model_calls() == [
        [
            "/tmp/loom-tests",
            name,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]
        for name, *_ in MODELS
    ], "every model must run once, alone, by exact name"


def test_builds_once_without_running_tests(tmp_path: Path) -> None:
    """The binary is built with the feature and `--no-run`, never by `cargo test`."""
    runner = FakeRunner()

    run(tmp_path, runner)

    builds = [command for command, _, _ in runner.calls if command[0] == "cargo" and command[1] == "test"]
    assert builds == [
        [
            "cargo",
            "test",
            "--locked",
            "--features",
            "loom-tests",
            "--lib",
            "--no-run",
            "--message-format=json",
        ]
    ], "the binary must be built exactly once, with --no-run"


def test_applies_the_profile_overlay_to_every_model(tmp_path: Path) -> None:
    """Every model sees the selected profile and the caller's environment."""
    runner = FakeRunner()

    run(tmp_path, runner, profile="scheduled", environment={"RUSTFLAGS": "-Zthreads=8"})

    model_environments = [
        environment
        for command, environment, _ in runner.calls
        if command[0] == "/tmp/loom-tests" and "--list" not in command
    ]
    assert model_environments, "the inventory must run at least one model"
    assert all(
        environment["PG_EMBED_LOOM_PROFILE"] == "scheduled"
        for environment in model_environments
    ), "every model must see the selected profile"
    build_environment = next(
        environment
        for command, environment, _ in runner.calls
        if command[0] == "cargo" and command[1] == "test"
    )
    assert build_environment["RUSTFLAGS"] == "-Zthreads=8", (
        "the build must inherit the caller's RUSTFLAGS"
    )


def test_applies_the_profile_deadline_to_every_model(tmp_path: Path) -> None:
    """The manifest's per-profile timeout is the model deadline."""
    runner = FakeRunner()

    run(tmp_path, runner, profile="scheduled")

    deadlines = [
        timeout
        for command, _, timeout in runner.calls
        if command[0] == "/tmp/loom-tests" and "--list" not in command
    ]
    assert deadlines == [60.0, 60.0], (
        "each model gets the profile's per-model ceiling when the budget allows"
    )


def test_caps_each_model_deadline_to_the_remaining_budget(tmp_path: Path) -> None:
    """A small whole-run budget shortens the deadline of a later model."""
    runner = FakeRunner(delay_seconds=1.5)

    run(tmp_path, runner, budgets={"pr": 2})

    deadlines = [
        timeout
        for command, _, timeout in runner.calls
        if command[0] == "/tmp/loom-tests" and "--list" not in command
    ]
    assert deadlines[0] <= 2.0, "the first model must not exceed the run budget"
    assert 0 < deadlines[1] < 1.0, (
        "the second model must be capped to the budget the first one left"
    )


def test_stops_starting_models_once_the_run_budget_is_spent(tmp_path: Path) -> None:
    """A spent budget records the rest as timeout and still writes a summary."""
    runner = FakeRunner(delay_seconds=1.2)

    report = run(tmp_path, runner, budgets={"pr": 1})
    loom_runner.write_summary(report, tmp_path / "report", {})

    assert len(runner.model_calls()) == 1, "no model may start after the budget"
    second = report.models[1]
    assert second.outcome == "timeout", "a model the budget never reached is a timeout"
    assert second.exit_code is None, "the model was never run"
    skipped_log = tmp_path / "report" / "logs" / log_name(MODELS[1][0])
    assert skipped_log.read_text().strip() == loom_runner.BUDGET_EXHAUSTED, (
        "the skipped model's log must explain the budget"
    )
    assert (tmp_path / "report" / "summary.json").exists(), (
        "the summary must be written even when the budget is spent"
    )


def test_writes_complete_summary_and_logs(tmp_path: Path) -> None:
    """The report names the commit, profile, bounds, tools and every model."""
    runner = FakeRunner()

    report = run(tmp_path, runner)
    loom_runner.write_summary(report, tmp_path / "report", {})

    document = json.loads((tmp_path / "report" / "summary.json").read_text())
    assert document["commit"] == "abc123", "the summary must name the commit"
    assert document["profile"] == "pr", "the summary must name the profile"
    assert document["settings"]["max_branches"] == 64, (
        "the summary must state the effective bounds"
    )
    assert document["tool_versions"]["loom"] == "0.7.2", "the summary must name Loom"
    assert document["counts"]["passed"] == 2, "both models must be counted as passed"
    assert document["success"] is True, "a passing inventory is a success"
    assert [model["name"] for model in document["models"]] == [
        name for name, *_ in MODELS
    ], "the summary must list every model in execution order"
    assert (tmp_path / "report" / "logs" / "env__loom_tests__first.log").exists(), (
        "each model must leave a log"
    )
    markdown = (tmp_path / "report" / "summary.md").read_text()
    assert "explored completely within the stated bounds" in markdown, (
        "the Markdown summary must state the bounds"
    )


def test_appends_to_the_job_summary_when_set(tmp_path: Path) -> None:
    """The Markdown summary reaches `GITHUB_STEP_SUMMARY` when it is set."""
    runner = FakeRunner()
    report = run(tmp_path, runner)
    step_summary = tmp_path / "step-summary.md"

    loom_runner.write_summary(
        report, tmp_path / "report", {"GITHUB_STEP_SUMMARY": str(step_summary)}
    )

    assert "## Loom model checking" in step_summary.read_text(), (
        "the job summary must receive the Markdown report"
    )


@pytest.mark.parametrize(
    "variable",
    [
        "LOOM_MAX_BRANCHES",
        "LOOM_MAX_PERMUTATIONS",
        "LOOM_MAX_DURATION",
        "LOOM_MAX_PREEMPTIONS",
        "LOOM_CHECKPOINT_FILE",
        "LOOM_CHECKPOINT_INTERVAL",
    ],
)
def test_rejects_limit_variables_before_building(tmp_path: Path, variable: str) -> None:
    """A stray Loom limit variable fails the run before anything is built."""
    runner = FakeRunner()

    with pytest.raises(DiscoveryError, match=variable):
        run(tmp_path, runner, environment={variable: "1"})

    assert runner.calls == [], "nothing may be built or run after the refusal"


def test_allows_log_and_location_variables(tmp_path: Path) -> None:
    """`LOOM_LOG` and `LOOM_LOCATION` are diagnostics, not bounds."""
    runner = FakeRunner()

    run(tmp_path, runner, environment={"LOOM_LOG": "1", "LOOM_LOCATION": "1"})

    assert runner.model_calls(), "diagnostic variables must not stop the run"


def test_rejects_a_build_without_the_loom_feature(tmp_path: Path) -> None:
    """An artefact built without `loom-tests` is not the inventory's binary."""
    runner = FakeRunner(features=())

    with pytest.raises(DiscoveryError, match="loom-tests"):
        run(tmp_path, runner)


def test_rejects_a_missing_expected_model(tmp_path: Path) -> None:
    """A model that disappears from the binary fails instead of being skipped."""
    runner = FakeRunner(listing=[MODELS[0][0]])

    with pytest.raises(DiscoveryError, match="not discoverable"):
        run(tmp_path, runner)


def test_rejects_an_unregistered_loom_model(tmp_path: Path) -> None:
    """An ignored Loom test outside the manifest fails the run."""
    runner = FakeRunner(listing=[name for name, *_ in MODELS] + ["env::loom_tests::extra"])

    with pytest.raises(DiscoveryError, match="not registered"):
        run(tmp_path, runner)


def test_classifies_a_killed_model_as_timeout(tmp_path: Path) -> None:
    """A model the seam killed at its deadline is a timeout, not a failure."""
    name = MODELS[0][0]
    runner = FakeRunner(
        outputs={name: Completed(-9, marker("loom-model-start", name), "", timed_out=True)}
    )

    report = run(tmp_path, runner)

    first = report.models[0]
    assert first.outcome == "timeout", "a killed model must be a timeout"
    assert first.exit_code is None, "a killed model has no exit code"
    assert report.is_success() is False, "a timed-out inventory is not a success"


def test_run_process_kills_a_child_at_its_deadline(tmp_path: Path) -> None:
    """The real process seam kills and reaps a child that outlives its deadline."""
    started = time.monotonic()

    completed = run_process(["sleep", "30"], tmp_path, dict(os.environ), 0.2)

    elapsed = time.monotonic() - started
    assert completed.timed_out is True, "the deadline must be reported"
    assert completed.returncode is not None and completed.returncode < 0, (
        "the child must be killed by a signal"
    )
    assert elapsed < 5, "the deadline must not wait for the child"


def test_propagates_a_non_zero_exit_as_failed(tmp_path: Path) -> None:
    """A model that exits non-zero is a failure."""
    name = MODELS[0][0]
    runner = FakeRunner(outputs={name: Completed(101, "assertion failed", "")})

    report = run(tmp_path, runner)

    assert report.models[0].outcome == "failed", "a non-zero exit is a failure"
    assert "assertion failed" in report.models[0].failure_tail, (
        "the failure tail must show why the model failed"
    )


def test_classifies_a_branch_limit_separately(tmp_path: Path) -> None:
    """Exhausting the branch budget is its own outcome, not a generic failure."""
    name = MODELS[0][0]
    runner = FakeRunner(
        outputs={
            name: Completed(
                101,
                "Model exceeded maximum number of branches. This is often caused by "
                "an algorithm requiring the processor to make progress, e.g. spin locks.",
                "",
            )
        }
    )

    report = run(tmp_path, runner)

    assert report.models[0].outcome == "branch_limit", (
        "an exhausted branch budget is its own outcome"
    )


def test_classifies_a_profile_mismatch_as_incomplete(tmp_path: Path) -> None:
    """A model that reports another profile did not run the requested bounds."""
    name = MODELS[0][0]
    runner = FakeRunner(outputs={name: Completed(0, passing_output(name, "scheduled"), "")})

    report = run(tmp_path, runner)

    assert report.models[0].outcome == "incomplete", (
        "a model reporting another profile did not run the requested bounds"
    )


def test_main_fails_when_no_model_was_executed(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """An empty report is a failure, never a vacuous pass."""
    empty = loom_runner.Report(
        commit="abc",
        profile="pr",
        settings=loom_runner.EffectiveSettings(3, 64, 3, "none", "none", "none"),
        tool_versions={},
        build_seconds=0.0,
        models=(),
        reproduction="make test-loom",
    )
    monkeypatch.setattr(loom_runner, "run_loom", lambda *args, **kwargs: empty)
    monkeypatch.setattr(loom_runner, "write_summary", lambda *args, **kwargs: None)

    assert loom_runner.main(profile="pr") == 1, "an empty report must fail the run"