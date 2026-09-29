"""Tests for the MSRV guard, and the contract that CI runs it.

The guard resolves a lockfile as a project on the declared `rust-version`
would and builds against it. These tests hold the order of its steps, that
the committed lockfile survives a failed build, and that `ci.yml` runs it
through `make msrv`, so deleting the job or the target fails a test.
"""

from __future__ import annotations

import importlib.util
import sys
import typing as typ
from pathlib import Path

import pytest
import yaml

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPO_ROOT / "scripts" / "msrv_check.py"
SPEC = importlib.util.spec_from_file_location("msrv_check", SCRIPT_PATH)
assert SPEC is not None, f"cannot build an import spec for {SCRIPT_PATH}"
msrv_check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None, f"import spec for {SCRIPT_PATH} has no loader"
sys.modules[SPEC.name] = msrv_check
SPEC.loader.exec_module(msrv_check)


def make_repo(root: Path, rust_version: str | None = "1.92") -> Path:
    """Write a manifest and lockfile into `root`."""
    line = f'rust-version = "{rust_version}"\n' if rust_version else ""
    (root / "Cargo.toml").write_text(f'[package]\nname = "x"\nversion = "0.1.0"\n{line}')
    (root / "Cargo.lock").write_text("committed\n")
    return root


class Recorder:
    """A runner that records commands and can fail on one of them."""

    def __init__(self, fail_on: str | None = None) -> None:
        """Remember which subcommand, if any, should fail."""
        self.commands: list[list[str]] = []
        self.fail_on = fail_on

    def __call__(self, command: list[str], repo: Path) -> None:
        """Record the command, rewrite the lockfile as Cargo would, maybe fail."""
        self.commands.append(command)
        if "generate-lockfile" in command:
            (repo / "Cargo.lock").write_text("resolved\n")
        if self.fail_on and self.fail_on in command:
            message = f"{self.fail_on} failed"
            raise RuntimeError(message)


def test_the_steps_install_resolve_then_check_at_the_declared_version(tmp_path: Path) -> None:
    """Toolchain first, then the resolved lock, then a locked build of every target."""
    recorder = Recorder()

    version = msrv_check.check_msrv(make_repo(tmp_path), recorder)

    assert version == "1.92"
    assert recorder.commands == [
        ["rustup", "toolchain", "install", "1.92", "--profile", "minimal"],
        ["cargo", "+1.92", "generate-lockfile"],
        ["cargo", "+1.92", "check", "--locked", "--all-targets", "--all-features"],
    ]


@pytest.mark.parametrize("failing", [None, "check"])
def test_the_committed_lockfile_comes_back(tmp_path: Path, failing: str | None) -> None:
    """A passing or a failing build leaves `Cargo.lock` as it was committed."""
    repo = make_repo(tmp_path)
    recorder = Recorder(fail_on=failing)

    if failing:
        with pytest.raises(RuntimeError):
            msrv_check.check_msrv(repo, recorder)
    else:
        msrv_check.check_msrv(repo, recorder)

    assert (repo / "Cargo.lock").read_text() == "committed\n"


def test_a_manifest_without_rust_version_is_refused(tmp_path: Path) -> None:
    """A check against no version would pass over everything, so it fails."""
    repo = make_repo(tmp_path, rust_version=None)

    with pytest.raises(SystemExit, match="rust-version"):
        msrv_check.check_msrv(repo, Recorder())


def test_the_resolver_prefers_releases_the_version_can_build() -> None:
    """The subprocess environment asks Cargo's resolver to fall back."""
    assert msrv_check.RESOLVER_VARIABLE == "CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS"


def jobs_running(command: str) -> list[str]:
    """Return the names of `ci.yml` jobs with a step that runs `command`."""
    document = yaml.safe_load((REPO_ROOT / ".github" / "workflows" / "ci.yml").read_text())
    return [
        name
        for name, job in document["jobs"].items()
        if any(step.get("run", "").strip() == command for step in job.get("steps", []))
    ]


def test_ci_runs_the_msrv_guard() -> None:
    """A job runs `make msrv`, so the guard cannot be dropped in silence."""
    assert jobs_running("make msrv"), "no ci.yml job runs `make msrv`"


def test_the_make_target_runs_the_script() -> None:
    """`make msrv` invokes `scripts/msrv_check.py`, not a lookalike."""
    makefile = (REPO_ROOT / "Makefile").read_text()
    recipe = makefile.split("\nmsrv:", 1)[1].split("\n\n", 1)[0]
    assert "scripts/msrv_check.py" in recipe


def test_the_declared_version_is_one_the_script_can_read() -> None:
    """The repository's own manifest declares a `rust-version`."""
    assert msrv_check.declared_rust_version(REPO_ROOT / "Cargo.toml")
