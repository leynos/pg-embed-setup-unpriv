"""Shared fakes and fixtures for the Loom runner's behavioural tests.

The runner is driven through an injected process seam, so the tests need a
replacement for every child process: the one-off build, the listing, and each
model. Keeping the fakes in their own module leaves the test module to state
the expectations, and lets a test tune one answer without rebuilding the seam.

Examples
--------
>>> passing_output("a::b").count("loom-model-")
2
"""

from __future__ import annotations

import json
import sys
import typing as typ
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

# The scripts are not a package: they are importable only after the path
# insertion above, so these imports cannot precede it.
import loom_runner  # noqa: E402
from loom_runner import Completed  # noqa: E402

#: The path the fake build reports for the test binary. Nothing executes it,
#: and it is deliberately outside `/tmp` so a lint rule cannot read it as a
#: hard-coded temporary directory.
FAKE_EXECUTABLE: typ.Final = "/opt/loom-fake/loom-tests"

__all__ = [
    "BUDGETS",
    "FAKE_EXECUTABLE",
    "MODELS",
    "Completed",
    "FakeAnswers",
    "FakeClock",
    "FakeRunner",
    "marker",
    "passing_output",
    "run",
    "seed_previous_report",
    "write_manifest",
]

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


class FakeClock:
    """A monotonic clock that only moves when a test advances it."""

    def __init__(self) -> None:
        """Start at zero."""
        self.now = 0.0

    def monotonic(self) -> float:
        """Return the current fake time."""
        return self.now

    def advance(self, seconds: float) -> None:
        """Move the clock forward by `seconds`."""
        self.now += seconds


@dataclass(frozen=True)
class FakeAnswers:
    """The canned replies the fake seam returns."""

    features: typ.Sequence[str] = ("loom-tests",)
    listing: typ.Sequence[str] | None = None
    outputs: typ.Mapping[str, Completed] = field(default_factory=dict)
    build_returncode: int = 0
    #: Fake time each model consumes; the clock advances by this much.
    delay_seconds: float = 0.0


class FakeRunner:
    """A process seam that answers the build, the listing and each model."""

    def __init__(
        self,
        models: typ.Sequence[tuple[str, str, int, int]] = MODELS,
        *,
        answers: FakeAnswers | None = None,
        clock: FakeClock | None = None,
    ) -> None:
        """Record the inventory, the canned answers and the clock to advance."""
        self.models = models
        self.answers = answers or FakeAnswers()
        self.clock = clock
        self.features = list(self.answers.features)
        listing = self.answers.listing
        self.listing = (
            list(listing) if listing is not None else [name for name, *_ in models]
        )
        self.outputs = dict(self.answers.outputs)
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
                "executable": FAKE_EXECUTABLE,
            }
            return Completed(
                self.answers.build_returncode, json.dumps(artefact) + "\n", ""
            )
        if "--list" in command:
            return Completed(0, "".join(f"{name}: test\n" for name in self.listing), "")
        name = command[1]
        if self.clock is not None:
            self.clock.advance(self.answers.delay_seconds)
        return self.outputs.get(name, Completed(0, passing_output(name), ""))

    def model_calls(self) -> list[list[str]]:
        """Return the commands that ran a model, not the listing."""
        return [
            command
            for command, _, _ in self.calls
            if command[0] == FAKE_EXECUTABLE and "--list" not in command
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


def seed_previous_report(output: Path) -> None:
    """Write the artefacts a finished run leaves behind."""
    (output / "logs").mkdir(parents=True)
    (output / "summary.json").write_text('{"success": true}', encoding="utf-8")
    (output / "summary.md").write_text("old summary", encoding="utf-8")
    (output / "build.log").write_text("old build", encoding="utf-8")
    (output / "logs" / "stale.log").write_text("old log", encoding="utf-8")