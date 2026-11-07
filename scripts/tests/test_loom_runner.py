"""Behavioural tests for the Loom runner.

The runner builds the test binary once, discovers the ignored models, checks
them against the manifest, and runs each one with an exact name filter and a
deadline. These tests drive it through an injected process seam, so they assert
the exact commands, the exact environment overlays, and the classification of
every outcome without compiling anything.
"""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

# The scripts are not a package: they are importable only after the path
# insertion above, so these imports cannot precede it.
import loom_runner  # noqa: E402
from loom_discovery import DiscoveryError  # noqa: E402
from loom_process import log_name, run_process  # noqa: E402

from loom_runner_fakes import (  # noqa: E402
    MODELS,
    Completed,
    FakeAnswers,
    FakeClock,
    FakeRunner,
    marker,
    passing_output,
    run,
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


def test_bounds_the_build_by_the_remaining_run_budget(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A hung build is killed at the run budget, not at the step ceiling."""
    clock = FakeClock()
    monkeypatch.setattr(loom_runner, "time", clock)
    runner = FakeRunner()

    run(tmp_path, runner, budgets={"pr": 90})

    build = next(
        timeout
        for command, _, timeout in runner.calls
        if command[0] == "cargo" and command[1] == "test"
    )
    assert build == 90.0, "the build must be capped at the remaining budget"


def test_caps_each_model_deadline_to_the_remaining_budget(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A small whole-run budget shortens the deadline of a later model."""
    clock = FakeClock()
    monkeypatch.setattr(loom_runner, "time", clock)
    runner = FakeRunner(answers=FakeAnswers(delay_seconds=1.5), clock=clock)

    run(tmp_path, runner, budgets={"pr": 2})

    deadlines = [
        timeout
        for command, _, timeout in runner.calls
        if command[0] == "/tmp/loom-tests" and "--list" not in command
    ]
    assert deadlines[0] == 2.0, "the first model must not exceed the run budget"
    assert deadlines[1] == 0.5, (
        "the second model must be capped to the budget the first one left"
    )


def test_stops_starting_models_once_the_run_budget_is_spent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A spent budget records the rest as timeout and still writes a summary."""
    clock = FakeClock()
    monkeypatch.setattr(loom_runner, "time", clock)
    runner = FakeRunner(answers=FakeAnswers(delay_seconds=1.2), clock=clock)

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
    runner = FakeRunner(answers=FakeAnswers(features=()))

    with pytest.raises(DiscoveryError, match="loom-tests"):
        run(tmp_path, runner)


def test_rejects_a_missing_expected_model(tmp_path: Path) -> None:
    """A model that disappears from the binary fails instead of being skipped."""
    runner = FakeRunner(answers=FakeAnswers(listing=[MODELS[0][0]]))

    with pytest.raises(DiscoveryError, match="not discoverable"):
        run(tmp_path, runner)


def test_rejects_an_unregistered_loom_model(tmp_path: Path) -> None:
    """An ignored Loom test outside the manifest fails the run."""
    runner = FakeRunner(
        answers=FakeAnswers(
            listing=[name for name, *_ in MODELS] + ["env::loom_tests::extra"]
        )
    )

    with pytest.raises(DiscoveryError, match="not registered"):
        run(tmp_path, runner)


def test_classifies_a_killed_model_as_timeout(tmp_path: Path) -> None:
    """A model the seam killed at its deadline is a timeout, not a failure."""
    name = MODELS[0][0]
    runner = FakeRunner(
        answers=FakeAnswers(
            outputs={
                name: Completed(
                    -9, marker("loom-model-start", name), "", timed_out=True
                )
            }
        )
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


@pytest.mark.parametrize(
    ("output", "exit_code", "timed_out", "expected"),
    [
        pytest.param(
            marker("loom-model-start", MODELS[0][0]),
            -9,
            True,
            "timeout",
            id="timeout",
        ),
        pytest.param("assertion failed", 101, False, "failed", id="failed"),
        pytest.param(
            loom_runner.BRANCH_LIMIT_TEXT, 101, False, "branch_limit", id="branch-limit"
        ),
        pytest.param(
            passing_output(MODELS[0][0], "scheduled"),
            0,
            False,
            "incomplete",
            id="profile-mismatch",
        ),
    ],
)
def test_classifies_each_outcome_once(
    tmp_path: Path,
    output: str,
    exit_code: int,
    timed_out: bool,
    expected: str,
) -> None:
    """Every outcome is decided by its own rule, in priority order."""
    name = MODELS[0][0]
    runner = FakeRunner(
        answers=FakeAnswers(
            outputs={
                name: Completed(exit_code, output, "", timed_out=timed_out),
            }
        )
    )

    report = run(tmp_path, runner)

    assert report.models[0].outcome == expected, (
        f"a {expected} model must be classified as {expected}"
    )


def test_a_killed_model_reports_no_exit_code(tmp_path: Path) -> None:
    """A killed model has no exit code and cannot make the run a success."""
    name = MODELS[0][0]
    runner = FakeRunner(
        answers=FakeAnswers(
            outputs={
                name: Completed(-9, marker("loom-model-start", name), "", timed_out=True)
            }
        )
    )

    report = run(tmp_path, runner)

    assert report.models[0].exit_code is None, "a killed model has no exit code"
    assert report.is_success() is False, "a timed-out inventory is not a success"


def test_a_failures_tail_shows_why_it_failed(tmp_path: Path) -> None:
    """The failure tail carries the output that explains the failure."""
    name = MODELS[0][0]
    runner = FakeRunner(
        answers=FakeAnswers(outputs={name: Completed(101, "assertion failed", "")})
    )

    report = run(tmp_path, runner)

    assert "assertion failed" in report.models[0].failure_tail, (
        "the failure tail must show why the model failed"
    )
