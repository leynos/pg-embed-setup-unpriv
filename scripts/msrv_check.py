"""Prove the crate builds with the Rust version its manifest declares.

`rust-version` is a promise to downstream users, and a dependency that needs
a newer compiler breaks it without any test noticing: the committed lockfile
resolves to the newest releases, which is what CI builds. 0.6.1 shipped with
`rust-version = "1.85"` while `postgresql_archive` 0.20.4 needed 1.94, so a
downstream project on 1.93 could not build it at all.

This check resolves a lockfile the way a project on the declared version
would (Cargo's rust-version-aware resolver, `fallback`), then builds every
target against it with that exact toolchain. The committed `Cargo.lock` is
put back afterwards, pass or fail.

Examples
--------
>>> commands_for("1.92")[1][:3]
['cargo', '+1.92', 'generate-lockfile']
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import typing as typ
from pathlib import Path

#: Makes Cargo prefer dependency releases the declared version can build.
RESOLVER_VARIABLE: typ.Final = "CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS"

#: Signature of the function that runs one command in the repository.
Runner: typ.TypeAlias = typ.Callable[[list[str], Path], None]


def declared_rust_version(manifest: Path) -> str:
    """Return the `package.rust-version` a manifest declares.

    Raises
    ------
    SystemExit
        When the manifest declares none, since a check against no version
        would pass over everything.
    """
    package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
    version = package.get("rust-version")
    if not isinstance(version, str) or not version:
        message = f"{manifest} declares no package.rust-version"
        raise SystemExit(message)
    return version


def commands_for(version: str) -> list[list[str]]:
    """Return the commands that install, resolve and build at `version`."""
    return [
        ["rustup", "toolchain", "install", version, "--profile", "minimal"],
        ["cargo", f"+{version}", "generate-lockfile"],
        ["cargo", f"+{version}", "check", "--locked", "--all-targets", "--all-features"],
    ]


def run_in(command: list[str], repo: Path) -> None:
    """Run one command in `repo`, failing loudly.

    `RUSTFLAGS` is cleared because the repository's Cargo configuration sets
    nightly-only flags, which a stable toolchain rejects. The resolver
    variable is set for every command; only `generate-lockfile` reads it.
    """
    environment = {**os.environ, RESOLVER_VARIABLE: "fallback", "RUSTFLAGS": ""}
    subprocess.run(command, cwd=repo, env=environment, check=True)  # noqa: S603


def check_msrv(repo: Path, runner: Runner = run_in) -> str:
    """Build `repo` at its declared `rust-version` against a resolved lockfile.

    The committed `Cargo.lock` is restored whatever happens, so a local run
    leaves the working tree as it found it.

    Returns
    -------
    str
        The version that was checked.
    """
    version = declared_rust_version(repo / "Cargo.toml")
    lockfile = repo / "Cargo.lock"
    with tempfile.TemporaryDirectory() as scratch:
        backup = Path(scratch) / "Cargo.lock"
        shutil.copy2(lockfile, backup)
        try:
            for command in commands_for(version):
                runner(command, repo)
        finally:
            shutil.copy2(backup, lockfile)
    return version


def main() -> int:
    """Check the repository at the current directory; return the exit code."""
    version = check_msrv(Path.cwd())
    sys.stdout.write(f"builds at the declared rust-version {version}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
