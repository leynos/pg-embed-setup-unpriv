"""Cargo command resolution and release binary builds."""

from __future__ import annotations

import shlex
from pathlib import Path
from typing import Protocol

from cuprum import ExecutionContext, Program, ProjectSettings, ProgramCatalogue
from cuprum import sh as cuprum_sh

from release_archive_staging import validate_release_spec_components

SHELL_QUOTES = frozenset({"'", '"'})
CARGO = Program("cargo")
CATALOGUE = ProgramCatalogue(
    projects=[
        ProjectSettings(
            name="cargo",
            programs=(CARGO,),
            documentation_locations=("https://doc.rust-lang.org/cargo/",),
            noise_rules=(),
        )
    ]
)


class ReleaseBuildSpecLike(Protocol):
    """Build inputs consumed by the Cargo release builder."""

    repo: Path
    target: str
    binaries: tuple[str, ...]
    cargo: str
    build_jobs: str | None


def build_release_binaries(spec: ReleaseBuildSpecLike) -> None:
    """Build the selected release binaries for `spec.target`."""
    validate_release_spec_components(spec.target, spec.binaries)
    program, program_args = _cargo_program_and_args(spec.cargo)
    args = [*program_args, "build", "--release", "--target", spec.target]
    args.extend(_cargo_build_job_args(spec.build_jobs))
    for binary in spec.binaries:
        args.extend(["--bin", binary])

    command = cuprum_sh.make(Program(program), catalogue=_catalogue_for(program))
    result = command(*args).run_sync(
        capture=False,
        echo=True,
        context=ExecutionContext(cwd=spec.repo),
    )
    if result.exit_code != 0:
        raise SystemExit(result.exit_code)


def _cargo_program_and_args(cargo: str) -> tuple[str, list[str]]:
    """Return the executable and wrapper arguments represented by `cargo`."""
    stripped_cargo = _normalise_cargo_command(cargo)
    if windows_wrapper := _windows_wrapper_program_and_args(stripped_cargo):
        return windows_wrapper
    return _resolve_cargo_program_and_args(
        stripped_cargo,
        _split_cargo_command(stripped_cargo),
    )


def _normalise_cargo_command(cargo: str) -> str:
    """Return a stripped, non-empty Cargo command string."""
    stripped_cargo = cargo.strip()
    if not stripped_cargo:
        raise SystemExit("cargo executable cannot be empty")
    return stripped_cargo


def _split_cargo_command(cargo: str, *, posix: bool = True) -> list[str]:
    """Split a Cargo command string into argv words."""
    try:
        cargo_command = shlex.split(cargo, posix=posix)
    except ValueError as err:
        raise SystemExit(f"invalid cargo executable command: {err}") from err
    if not cargo_command:
        raise SystemExit("cargo executable cannot be empty")
    return cargo_command


def _resolve_cargo_program_and_args(
    stripped_cargo: str,
    cargo_command: list[str],
) -> tuple[str, list[str]]:
    """Resolve a parsed Cargo command into a program and wrapper arguments."""
    if path_wrapper := _path_wrapper_program_and_args(cargo_command):
        return path_wrapper
    if _looks_like_executable_path(stripped_cargo):
        return _strip_matching_quotes(stripped_cargo), []
    program, *program_args = cargo_command
    return program, program_args


def _windows_wrapper_program_and_args(cargo: str) -> tuple[str, list[str]] | None:
    """Return a Windows `.exe` wrapper command when `cargo` includes arguments."""
    windows_command = _split_cargo_command(cargo, posix=False)
    windows_program = _strip_matching_quotes(windows_command[0])
    if len(windows_command) > 1 and windows_program.lower().endswith(".exe"):
        return windows_program, windows_command[1:]
    return None


def _path_wrapper_program_and_args(cargo_command: list[str]) -> tuple[str, list[str]] | None:
    """Return a path-like wrapper command when `cargo` was split as argv."""
    if len(cargo_command) > 1 and _looks_like_executable_path(cargo_command[0]):
        program, *program_args = cargo_command
        return _strip_matching_quotes(program), program_args
    return None


def _looks_like_executable_path(cargo: str) -> bool:
    """Return whether `cargo` names a path instead of a wrapper argv string."""
    executable = _strip_matching_quotes(cargo)
    return "/" in executable or "\\" in executable or executable.lower().endswith(".exe")


def _strip_matching_quotes(value: str) -> str:
    """Strip one matching shell-quote pair around an executable path."""
    if _has_matching_outer_quotes(value):
        return value[1:-1]
    return value


def _has_matching_outer_quotes(value: str) -> bool:
    """Return whether `value` is enclosed in one matching shell quote pair."""
    if len(value) < 2:
        return False
    first = value[0]
    last = value[-1]
    return first == last and first in SHELL_QUOTES


def _cargo_build_job_args(build_jobs: str | None) -> list[str]:
    """Return Cargo arguments represented by the Makefile `BUILD_JOBS` value."""
    if not build_jobs:
        return []
    parts = shlex.split(build_jobs)
    if len(parts) == 1 and parts[0].isdecimal():
        return ["--jobs", parts[0]]
    return parts


def _catalogue_for(cargo: str) -> ProgramCatalogue:
    """Return a command catalogue that permits the configured Cargo binary."""
    if cargo == str(CARGO):
        return CATALOGUE
    return ProgramCatalogue(
        projects=[
            ProjectSettings(
                name="cargo",
                programs=(Program(cargo),),
                documentation_locations=("https://doc.rust-lang.org/cargo/",),
                noise_rules=(),
            )
        ]
    )
