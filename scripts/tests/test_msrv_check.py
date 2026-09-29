"""Tests for the MSRV guard, and the contract that CI runs it.

The guard resolves a lockfile as a project on the declared `rust-version`
would and builds against it. These tests hold the order of its steps, that
the committed lockfile survives a failed build, and that `ci.yml` runs it
through `make msrv`, so deleting the job or the target fails a test.
"""

from __future__ import annotations

import importlib.util
import os
import subprocess
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

    with pytest.raises(msrv_check.ManifestError, match="rust-version"):
        msrv_check.check_msrv(repo, Recorder())


def test_a_manifest_that_is_not_toml_is_named_as_such() -> None:
    """Parsing reports a malformed manifest as a `ManifestError`, not a traceback."""
    with pytest.raises(msrv_check.ManifestError, match="not valid TOML"):
        msrv_check.parse_rust_version("[package\n", "Cargo.toml")


def test_a_missing_manifest_is_a_manifest_error(tmp_path: Path) -> None:
    """Reading reports an unreadable file through the same named error."""
    with pytest.raises(msrv_check.ManifestError, match="cannot read"):
        msrv_check.read_manifest(tmp_path / "Cargo.toml")


def test_the_reader_is_injected() -> None:
    """The declared version comes from whatever the reader returns."""
    version = msrv_check.declared_rust_version(
        Path("Cargo.toml"), reader=lambda _path: '[package]\nrust-version = "1.99"\n'
    )
    assert version == "1.99"


def test_the_resolver_prefers_releases_the_version_can_build() -> None:
    """The subprocess environment asks Cargo's resolver to fall back."""
    assert msrv_check.RESOLVER_VARIABLE == "CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS"


@pytest.mark.parametrize("override", msrv_check.COMPILER_OVERRIDES)
def test_an_inherited_compiler_cannot_replace_the_declared_one(override: str) -> None:
    """Cargo honours `RUSTC` over `+<version>`, so the check must not inherit it."""
    inherited = {override: "/opt/newer/rustc", "HOME": "/home/x", "RUSTFLAGS": "-D warnings"}

    environment = msrv_check.subprocess_environment(inherited)

    assert override not in environment
    assert environment["HOME"] == "/home/x"
    assert environment["RUSTFLAGS"] == ""
    assert environment[msrv_check.RESOLVER_VARIABLE] == "fallback"


def test_run_in_passes_that_environment_to_the_subprocess(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The command runs in the repository, with the cleaned environment, and must succeed."""
    monkeypatch.setenv("RUSTC", "/opt/newer/rustc")
    monkeypatch.setenv(msrv_check.RESOLVER_VARIABLE, "error")
    calls: list[tuple[list[str], dict[str, typ.Any]]] = []
    monkeypatch.setattr(
        msrv_check.subprocess,
        "run",
        lambda command, **options: calls.append((command, options)),
    )

    msrv_check.run_in(["cargo", "check"], tmp_path)

    [(command, options)] = calls
    assert command == ["cargo", "check"]
    assert options["cwd"] == tmp_path
    assert options["check"] is True
    assert "RUSTC" not in options["env"]
    assert options["env"][msrv_check.RESOLVER_VARIABLE] == "fallback"
    assert options["env"]["RUSTFLAGS"] == ""


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


FAKE_TOOL = """#!/bin/sh
# Records one line per call: tool, arguments, directory, and the variables the
# check must control. `cargo generate-lockfile` rewrites the lockfile, and a
# check fails when FAKE_FAIL names it.
printf '%s|%s|%s|flags=[%s]|rustc=[%s]|resolver=[%s]\\n' \
  "$(basename "$0")" "$*" "$PWD" "$RUSTFLAGS" "${RUSTC-unset}" \
  "$CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS" >> "$FAKE_LOG"
case "$*" in
  *generate-lockfile*) echo resolved > Cargo.lock ;;
esac
case "$*" in
  *"$FAKE_FAIL"*) exit 1 ;;
esac
"""


def run_cli(repo: Path, bin_dir: Path, fail_on: str) -> tuple[subprocess.CompletedProcess[str], list[str]]:
    """Run `scripts/msrv_check.py` in `repo` with fake `rustup` and `cargo` first on PATH."""
    bin_dir.mkdir()
    for tool in ("rustup", "cargo"):
        path = bin_dir / tool
        path.write_text(FAKE_TOOL)
        path.chmod(0o755)
    log = repo / "calls.log"
    environment = {
        **os.environ,
        "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
        "FAKE_LOG": str(log),
        "FAKE_FAIL": fail_on,
        "RUSTC": "/opt/newer/rustc",
        "RUSTFLAGS": "-D warnings",
    }
    result = subprocess.run(  # noqa: S603
        [sys.executable, str(SCRIPT_PATH)],
        cwd=repo,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    lines = log.read_text().splitlines() if log.exists() else []
    return result, lines


def test_the_command_line_runs_the_steps_in_the_repository(tmp_path: Path) -> None:
    """End to end through `main`: exit 0, the declared toolchain, the controlled environment."""
    (tmp_path / "repo").mkdir()
    repo = make_repo(tmp_path / "repo")

    result, calls = run_cli(repo, tmp_path / "bin", fail_on="never-matches")

    assert result.returncode == 0, result.stderr
    assert "builds at the declared rust-version 1.92" in result.stdout
    assert [call.split("|")[0:2] for call in calls] == [
        ["rustup", "toolchain install 1.92 --profile minimal"],
        ["cargo", "+1.92 generate-lockfile"],
        ["cargo", "+1.92 check --locked --all-targets --all-features"],
    ]
    for call in calls:
        _tool, _args, directory, flags, compiler, resolver = call.split("|")
        assert Path(directory).resolve() == repo.resolve()
        assert flags == "flags=[]"
        assert compiler == "rustc=[unset]"
        assert resolver == "resolver=[fallback]"
    assert (repo / "Cargo.lock").read_text() == "committed\n"


def test_a_failing_build_fails_the_command_and_restores_the_lockfile(tmp_path: Path) -> None:
    """A failed `cargo check` exits 1, names the command, and leaves the lock as committed."""
    (tmp_path / "repo").mkdir()
    repo = make_repo(tmp_path / "repo")

    result, _calls = run_cli(repo, tmp_path / "bin", fail_on="check")

    assert result.returncode == 1
    assert "check" in result.stderr
    assert (repo / "Cargo.lock").read_text() == "committed\n"


def test_a_manifest_without_rust_version_exits_with_two(tmp_path: Path) -> None:
    """The command line reports the named manifest error and runs nothing."""
    (tmp_path / "repo").mkdir()
    repo = make_repo(tmp_path / "repo", rust_version=None)

    result, calls = run_cli(repo, tmp_path / "bin", fail_on="never-matches")

    assert result.returncode == 2
    assert "rust-version" in result.stderr
    assert calls == []
