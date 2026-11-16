"""Behavioural tests for the Loom runner's report writing.

The runner clears the report directory before it does anything else, then
writes `summary.json`, `summary.md` and the job summary once every model has
run. These tests hold those two invariants: a refused run must not publish an
earlier success, and a finished run must always publish its own summary.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

# The scripts are not a package: they are importable only after the path
# insertion above, so these imports cannot precede it.
import loom_runner  # noqa: E402
from loom_discovery import DiscoveryError  # noqa: E402

from loom_runner_fakes import (  # noqa: E402
    MODELS,
    Completed,
    FakeAnswers,
    FakeRunner,
    run,
    seed_previous_report,
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


def test_clears_a_previous_report_before_running(tmp_path: Path) -> None:
    """An earlier run's artefacts never survive into the new report."""
    output = tmp_path / "report"
    seed_previous_report(output)
    runner = FakeRunner()

    report = run(tmp_path, runner)
    loom_runner.write_summary(report, output, {})

    document = json.loads((output / "summary.json").read_text())
    assert document["commit"] == "abc123", "the summary must describe this run"
    assert not (output / "logs" / "stale.log").exists(), (
        "a log from an earlier run must not be published with this one"
    )


def test_a_refused_run_leaves_no_previous_summary(tmp_path: Path) -> None:
    """A run refused before building must not publish an earlier success."""
    output = tmp_path / "report"
    seed_previous_report(output)

    with pytest.raises(DiscoveryError, match="LOOM_MAX_BRANCHES"):
        run(tmp_path, FakeRunner(), environment={"LOOM_MAX_BRANCHES": "1"})

    assert not (output / "summary.json").exists(), (
        "a refused run must not leave an earlier summary behind"
    )
    assert not (output / "logs" / "stale.log").exists(), (
        "a refused run must clear the earlier run's logs too"
    )


def test_survives_a_malformed_start_marker(tmp_path: Path) -> None:
    """A truncated marker leaves the bounds unknown instead of aborting."""
    malformed = "loom-model-start profile=pr"
    runner = FakeRunner(
        answers=FakeAnswers(
            outputs={name: Completed(0, malformed, "") for name, *_ in MODELS}
        )
    )

    report = run(tmp_path, runner)
    loom_runner.write_summary(report, tmp_path / "report", {})

    assert report.settings.max_threads == 0, "an unreadable marker leaves no bounds"
    assert (tmp_path / "report" / "summary.json").exists(), (
        "the summary must be written even when a marker cannot be read"
    )


def test_main_fails_when_no_model_was_executed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
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