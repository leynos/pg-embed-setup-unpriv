"""Run the Loom processes and describe the environment they see.

Every external effect the runner needs lives here: starting a child with a
deadline, composing the commands, overlaying the profile, and reading the
facts the summary records. Keeping the seam in one module lets the behavioural
tests replace it with a fake and assert the exact commands and environments.

Examples
--------
>>> build_command("cargo")[:3]
['cargo', 'test', '--locked']
>>> model_command(Path("/tmp/t"), "a::b")[1:3]
['a::b', '--exact']
"""

from __future__ import annotations

import subprocess
import typing as typ
from dataclasses import dataclass
from pathlib import Path

from loom_discovery import LOOM_FEATURE

#: Variables that would silently change Loom's exploration bounds.
REJECTED_VARIABLES: typ.Final = (
    "LOOM_MAX_BRANCHES",
    "LOOM_MAX_PERMUTATIONS",
    "LOOM_MAX_DURATION",
    "LOOM_MAX_PREEMPTIONS",
    "LOOM_CHECKPOINT_FILE",
    "LOOM_CHECKPOINT_INTERVAL",
)

#: The variable the Rust model runner reads to select a profile.
PROFILE_VARIABLE: typ.Final = "PG_EMBED_LOOM_PROFILE"


@dataclass(frozen=True)
class Completed:
    """The result of one child process.

    Attributes
    ----------
    returncode : int
        The exit code, negative when a signal ended the process.
    stdout : str
        The captured standard output.
    stderr : str
        The captured standard error.
    timed_out : bool
        Whether the deadline killed the process.
    """

    returncode: int
    stdout: str
    stderr: str
    timed_out: bool = False


#: Signature of the function that runs one child process.
Runner: typ.TypeAlias = typ.Callable[
    [list[str], Path, typ.Mapping[str, str], float | None], Completed
]


def run_process(
    command: list[str],
    cwd: Path,
    environment: typ.Mapping[str, str],
    timeout: float | None = None,
) -> Completed:
    """Run `command` in `cwd`, killing and reaping it at `timeout`.

    Examples
    --------
    >>> run_process(["true"], Path.cwd(), {}).returncode
    0
    """
    process = subprocess.Popen(  # noqa: S603
        command,
        cwd=cwd,
        env=dict(environment),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        stdout, stderr = process.communicate()
        return Completed(process.returncode, stdout, stderr, timed_out=True)
    return Completed(process.returncode, stdout, stderr)


def rejected_variables(environment: typ.Mapping[str, str]) -> list[str]:
    """Return the Loom limit variables present in `environment`.

    Examples
    --------
    >>> rejected_variables({"LOOM_MAX_BRANCHES": "1", "LOOM_LOG": "1"})
    ['LOOM_MAX_BRANCHES']
    """
    return sorted(name for name in REJECTED_VARIABLES if name in environment)


def build_command(cargo: str) -> list[str]:
    """Return the command that builds the test binary once.

    Examples
    --------
    >>> build_command("cargo")[:3]
    ['cargo', 'test', '--locked']
    """
    return [
        cargo,
        "test",
        "--locked",
        "--features",
        LOOM_FEATURE,
        "--lib",
        "--no-run",
        "--message-format=json",
    ]


def model_command(executable: Path, name: str) -> list[str]:
    """Return the command that runs exactly one model.

    Examples
    --------
    >>> model_command(Path("/tmp/t"), "a::b")[1:3]
    ['a::b', '--exact']
    """
    return [
        str(executable),
        name,
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]


def build_environment(inherited: typ.Mapping[str, str]) -> dict[str, str]:
    """Return the build environment, forwarding `RUSTFLAGS` as an overlay.

    The Makefile composes the repository's standard flags into `RUSTFLAGS`
    before invoking this script, so the value is forwarded explicitly rather
    than left to inheritance.

    Examples
    --------
    >>> build_environment({"RUSTFLAGS": "-Zthreads=8"})["RUSTFLAGS"]
    '-Zthreads=8'
    """
    environment = dict(inherited)
    if "RUSTFLAGS" in inherited:
        environment["RUSTFLAGS"] = inherited["RUSTFLAGS"]
    return environment


def model_environment(
    inherited: typ.Mapping[str, str], profile: str
) -> dict[str, str]:
    """Return the environment one model runs in.

    Examples
    --------
    >>> model_environment({}, "scheduled")["PG_EMBED_LOOM_PROFILE"]
    'scheduled'
    """
    return {**inherited, PROFILE_VARIABLE: profile}


def loom_version(lockfile: str) -> str:
    """Return the `loom` version pinned in a `Cargo.lock` document.

    Examples
    --------
    >>> loom_version('[[package]]\\nname = "loom"\\nversion = "0.7.2"\\n')
    '0.7.2'
    >>> loom_version("")
    'unknown'
    """
    for block in lockfile.split("[[package]]"):
        if 'name = "loom"' not in block:
            continue
        for line in block.splitlines():
            if line.startswith("version = "):
                return line.split('"')[1]
    return "unknown"


def tool_version(
    runner: Runner, program: str, repo: Path, environment: typ.Mapping[str, str]
) -> str:
    """Return the first line of `program --version`, or `unknown`."""
    completed = runner([program, "--version"], repo, environment, 60)
    first = completed.stdout.strip().splitlines()
    return first[0] if completed.returncode == 0 and first else "unknown"


def log_name(name: str) -> str:
    """Return a filesystem-safe log name for a model."""
    return name.replace("::", "__") + ".log"


def commit_under_test(repo: Path, environment: typ.Mapping[str, str]) -> str:
    """Return the commit under test."""
    if sha := environment.get("GITHUB_SHA"):
        return sha
    completed = subprocess.run(  # noqa: S603
        ["git", "rev-parse", "HEAD"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    return completed.stdout.strip() or "unknown"
