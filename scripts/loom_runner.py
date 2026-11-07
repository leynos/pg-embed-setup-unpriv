#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.13"
# dependencies = [
#   "cyclopts==4.19.0",
# ]
# ///
"""Run the Loom model inventory under an explicit exploration profile.

The runner builds the library test binary once with the `loom-tests` feature,
reads the ignored test names it reports, checks them against
`.config/loom-models.toml`, and then runs each model on its own with an exact
name filter and a wall-clock deadline. Every model is classified exactly once
as `passed`, `failed`, `branch_limit`, `timeout` or `incomplete`, and the run
fails unless every expected model passed. A per-profile aggregate budget caps
the whole run below the workflow step ceiling, so even a pathological profile
still writes its summary.

A pass means the model explored completely within the stated bounds. It is not
a proof for schedules outside those bounds.

Example
-------
Run the pull-request profile and write the report under `target/loom-report`:

```
uv run --script scripts/loom_runner.py --profile pr
```

See Also
--------
`loom_manifest` parses the inventory, `loom_discovery` finds the compiled test
binary, `loom_process` runs the children, and `loom_report` renders the
summary.
"""

from __future__ import annotations

import os
import shutil
import sys
import time
import typing as typ
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Annotated

from cyclopts import App, Parameter

from loom_discovery import (
    parse_cargo_messages,
    parse_listing,
    select_test_executable,
)
from loom_manifest import (
    DiscoveryError,
    Manifest,
    Model,
    load_manifest,
    validate_inventory,
)
from loom_process import (
    Completed,
    Runner,
    build_command,
    build_environment,
    commit_under_test,
    log_name,
    loom_version,
    model_command,
    model_environment,
    rejected_variables,
    run_process,
    tool_version,
)
from loom_report import (
    BRANCH_LIMIT_TEXT,
    EffectiveSettings,
    ModelResult,
    Report,
    failure_tail,
    parse_marker,
    render_json,
    render_markdown,
    settings_from_marker,
)

#: The default report directory, uploaded by the workflow.
DEFAULT_OUTPUT_DIR: typ.Final = Path("target/loom-report")

#: Written as the log and failure tail of a model the run budget never reached.
BUDGET_EXHAUSTED: typ.Final = (
    "the aggregate run budget was exhausted before this model started"
)

#: The files one run writes at the top level of the report directory.
REPORT_ARTEFACTS: typ.Final = ("summary.json", "summary.md", "build.log")


def _reset_report(output_dir: Path) -> Path:
    """Clear a previous run's report and return the log directory.

    Discovery can refuse a run before `write_summary` is reached. The
    workflow uploads the report directory even then, so a stale summary would
    be published beside a failing job and could still say the last run
    passed.
    """
    output_dir.mkdir(parents=True, exist_ok=True)
    for name in REPORT_ARTEFACTS:
        (output_dir / name).unlink(missing_ok=True)
    logs = output_dir / "logs"
    shutil.rmtree(logs, ignore_errors=True)
    logs.mkdir()
    return logs


def _marker_positions(output: str) -> tuple[int, int] | None:
    """Return the line numbers of the single start and complete markers."""
    starts: list[int] = []
    completes: list[int] = []
    for index, line in enumerate(output.splitlines()):
        fields = parse_marker(line)
        if fields is None:
            continue
        (starts if fields["kind"] == "start" else completes).append(index)
    if len(starts) != 1 or len(completes) != 1:
        return None
    return starts[0], completes[0]


def _markers_agree(output: str, name: str, profile: str) -> bool:
    """Return whether both markers name `name` and `profile`, in order."""
    positions = _marker_positions(output)
    if positions is None or positions[0] >= positions[1]:
        return False
    markers = [fields for line in output.splitlines() if (fields := parse_marker(line))]
    return all(
        marker.get("test") == name and marker.get("profile") == profile
        for marker in markers
    )


def _libtest_passed(output: str, name: str) -> bool:
    """Return whether libtest ran exactly one test and it passed.

    `--nocapture` lets the model's markers interleave with libtest's progress
    line, so the result word is not always on the `test <name> ...` line. The
    line prefix and the summary's `1 passed; 0 failed` together prove that the
    named test was the one that ran and that it passed.
    """
    lines = [line.strip() for line in output.splitlines()]
    executed = [line for line in lines if line.startswith(f"test {name} ...")]
    summary = [
        line
        for line in lines
        if line.startswith("test result: ok. 1 passed; 0 failed;")
    ]
    return len(executed) == 1 and len(summary) == 1


def classify(
    output: str,
    exit_code: int,
    timed_out: bool,
    name: str,
    profile: str,
) -> str:
    """Return the single outcome for one model run.

    Parameters
    ----------
    output : str
        The captured standard output and standard error of the model.
    exit_code : int
        The process exit code, negative when a signal ended it.
    timed_out : bool
        Whether the deadline killed the model.
    name : str
        The model's full libtest name, which the markers must repeat.
    profile : str
        The profile the runner asked for, which the markers must repeat.

    Returns
    -------
    str
        One of `timeout`, `branch_limit`, `failed`, `incomplete` or `passed`,
        decided in that priority order.

    Examples
    --------
    >>> classify("", 0, True, "a::b", "pr")
    'timeout'
    >>> classify("Model exceeded maximum number of branches", 101, False, "a::b", "pr")
    'branch_limit'
    >>> classify("", 1, False, "a::b", "pr")
    'failed'
    >>> classify("", 0, False, "a::b", "pr")
    'incomplete'
    """
    if timed_out:
        return "timeout"
    if BRANCH_LIMIT_TEXT in output:
        return "branch_limit"
    if exit_code != 0:
        return "failed"
    if not _markers_agree(output, name, profile):
        return "incomplete"
    if not _libtest_passed(output, name):
        return "incomplete"
    return "passed"


def _budget_exhausted(model: Model, log: str, logs: Path) -> ModelResult:
    """Record a model the aggregate budget did not leave time to run."""
    (logs / log).write_text(BUDGET_EXHAUSTED + "\n", encoding="utf-8")
    return ModelResult(
        name=model.name,
        suite=model.suite,
        spawned_threads=model.spawned_threads,
        context_threads=model.context_threads,
        outcome="timeout",
        duration_seconds=0.0,
        exit_code=None,
        log=f"logs/{log}",
        failure_tail=BUDGET_EXHAUSTED,
    )


def _run_model(
    model: Model,
    executable: Path,
    profile: str,
    ceiling: float,
    deadline: float,
    repo: Path,
    environment: Mapping[str, str],
    runner: Runner,
    logs: Path,
) -> ModelResult:
    """Run one model within the profile's per-model and whole-run limits.

    The per-model ceiling bounds a single model; the aggregate deadline bounds
    the run, so a pathological profile still reaches `write_summary` before the
    workflow step ceiling cancels it.
    """
    log = log_name(model.name)
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        return _budget_exhausted(model, log, logs)
    started = time.monotonic()
    completed = runner(
        model_command(executable, model.name),
        repo,
        environment,
        min(remaining, ceiling),
    )
    duration = time.monotonic() - started
    output = completed.stdout + completed.stderr
    (logs / log).write_text(output, encoding="utf-8")
    outcome = classify(
        output, completed.returncode, completed.timed_out, model.name, profile
    )
    return ModelResult(
        name=model.name,
        suite=model.suite,
        spawned_threads=model.spawned_threads,
        context_threads=model.context_threads,
        outcome=outcome,
        duration_seconds=duration,
        exit_code=None if completed.timed_out else completed.returncode,
        log=f"logs/{log}",
        failure_tail="" if outcome == "passed" else failure_tail(output),
    )


def _settings_from_output(output: str) -> EffectiveSettings | None:
    """Return the settings the first start marker reported, if any."""
    for line in output.splitlines():
        fields = parse_marker(line)
        if fields is not None and fields["kind"] == "start":
            return settings_from_marker(fields)
    return None


def _settings_from_logs(logs: Path, models: Sequence[Model]) -> EffectiveSettings:
    """Return the settings any model reported, or a sentinel when none did.

    A model killed before it printed its start marker leaves the bounds
    unknown; the report says so rather than inventing them.
    """
    for model in models:
        settings = _settings_from_output(
            (logs / log_name(model.name)).read_text(encoding="utf-8")
        )
        if settings is not None:
            return settings
    return EffectiveSettings(0, 0, None, "unknown", "unknown", "unknown")


def run_loom(
    repo: Path,
    profile: str,
    manifest_path: Path,
    output_dir: Path,
    cargo: str,
    runner: Runner = run_process,
    environment: Mapping[str, str] | None = None,
) -> Report:
    """Build, discover, validate and run every model, returning the report.

    Parameters
    ----------
    repo : Path
        The repository root, used as the working directory of every child.
    profile : str
        The exploration profile, `pr` or `scheduled`.
    manifest_path : Path
        The model inventory to load.
    output_dir : Path
        The directory that receives `build.log`, the per-model logs and the
        summaries. Its previous contents are cleared before anything else, so
        a refused run cannot leave an earlier summary to be uploaded.
    cargo : str
        The Cargo executable.
    runner : Runner
        The process seam; the tests replace it with a fake.
    environment : Mapping[str, str] | None
        The inherited environment, or `None` to use `os.environ`.

    Returns
    -------
    Report
        One result per expected model, in execution order, with the settings,
        tool versions and commit the run observed.

    Raises
    ------
    DiscoveryError
        If the environment, manifest, build or inventory cannot be trusted.
    """
    inherited = dict(os.environ if environment is None else environment)
    logs = _reset_report(output_dir)
    if rejected := rejected_variables(inherited):
        raise DiscoveryError(
            "refusing to run with Loom limit variables set: " + ", ".join(rejected)
        )
    manifest: Manifest = load_manifest(manifest_path)
    if profile not in manifest.timeouts:
        raise DiscoveryError(f"unknown Loom profile {profile!r}")
    deadline = time.monotonic() + float(manifest.budgets[profile])

    build_started = time.monotonic()
    # A hung build would otherwise run past the whole-run budget and be killed
    # by the workflow step before `build.log` was written.
    build = runner(
        build_command(cargo),
        repo,
        build_environment(inherited),
        max(deadline - build_started, 1.0),
    )
    build_seconds = time.monotonic() - build_started
    (output_dir / "build.log").write_text(
        build.stdout + build.stderr, encoding="utf-8"
    )
    if build.returncode != 0:
        raise DiscoveryError(
            f"the Loom test binary failed to build; see {output_dir / 'build.log'}"
        )
    executable = select_test_executable(parse_cargo_messages(build.stdout))

    listing = runner(
        [str(executable), "--list", "--ignored"], repo, inherited, 120
    )
    if listing.returncode != 0:
        raise DiscoveryError("the test binary could not list its ignored tests")
    validate_inventory(manifest, parse_listing(listing.stdout), profile)

    model_environment_ = model_environment(inherited, profile)
    ceiling = float(manifest.timeouts[profile])
    results = tuple(
        _run_model(
            model,
            executable,
            profile,
            ceiling,
            deadline,
            repo,
            model_environment_,
            runner,
            logs,
        )
        for model in manifest.models
    )
    settings = _settings_from_logs(logs, manifest.models)
    lockfile = (repo / "Cargo.lock").read_text(encoding="utf-8")
    return Report(
        commit=commit_under_test(repo, inherited),
        profile=profile,
        settings=settings,
        tool_versions={
            "cargo": tool_version(runner, cargo, repo, inherited),
            "rustc": tool_version(runner, "rustc", repo, inherited),
            "loom": loom_version(lockfile),
        },
        build_seconds=build_seconds,
        models=results,
        reproduction=(
            f"make test-loom LOOM_PROFILE={profile} "
            f"LOOM_OUTPUT_DIR={output_dir}"
        ),
    )


def write_summary(report: Report, output_dir: Path, environment: Mapping[str, str]) -> None:
    """Write `summary.json` and `summary.md`, and append to the job summary.

    Parameters
    ----------
    report : Report
        The finished run to render.
    output_dir : Path
        The directory that receives both summaries.
    environment : Mapping[str, str]
        The environment whose `GITHUB_STEP_SUMMARY`, when set, receives the
        Markdown summary as well.

    Returns
    -------
    None
        The summaries are written as a side effect.
    """
    (output_dir / "summary.json").write_text(render_json(report), encoding="utf-8")
    markdown = render_markdown(report)
    (output_dir / "summary.md").write_text(markdown, encoding="utf-8")
    if step_summary := environment.get("GITHUB_STEP_SUMMARY"):
        with Path(step_summary).open("a", encoding="utf-8") as handle:
            handle.write(markdown + "\n")


app = App(help=__doc__)

_ProfileArg = Annotated[
    typ.Literal["pr", "scheduled"],
    Parameter(name="--profile", help="Exploration profile to run."),
]
_ManifestArg = Annotated[
    Path, Parameter(name="--manifest", help="Path to the model inventory.")
]
_OutputArg = Annotated[
    Path, Parameter(name="--output-dir", help="Directory for logs and summaries.")
]
_CargoArg = Annotated[str, Parameter(name="--cargo", help="Cargo executable.")]


@app.default
def main(
    *,
    profile: _ProfileArg = "pr",
    manifest: _ManifestArg = Path(".config/loom-models.toml"),
    output_dir: _OutputArg = DEFAULT_OUTPUT_DIR,
    cargo: _CargoArg = "cargo",
) -> int:
    """Run the Loom inventory and return the process exit code.

    Parameters
    ----------
    profile : str
        The exploration profile to run.
    manifest : Path
        The model inventory to load.
    output_dir : Path
        The directory that receives logs and summaries.
    cargo : str
        The Cargo executable.

    Returns
    -------
    int
        `0` when every expected model passed, `1` when none ran or one did
        not pass, and `2` when discovery refused the run.

    Examples
    --------
    >>> main(profile="pr")  # doctest: +SKIP
    """
    try:
        report = run_loom(Path.cwd(), profile, manifest, output_dir, cargo)
    except DiscoveryError as err:
        sys.stderr.write(f"loom_runner: {err}\n")
        return 2
    write_summary(report, output_dir, os.environ)
    if not report.models:
        sys.stderr.write("loom_runner: no models were executed\n")
        return 1
    if not report.is_success():
        sys.stderr.write("loom_runner: one or more models did not pass\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(app())
