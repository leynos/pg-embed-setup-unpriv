"""Run the pinned Markdown lint CLI against fixtures, as the gate does.

The contract tests judge the Makefile, workflow and configuration as text,
so they can pass while the command they describe still rejects the wrong
files. This test executes the same pinned CLI the Makefile runs, in a
temporary tree holding a copy of the repository's configuration, and
asserts the behaviour the contract assumes:

- a valid document passes;
- an invalid document fails and names itself;
- a document under an ignored directory is never linted;
- a caller-supplied ``**/*.md`` selects exactly what the configuration
  selects alone, which is why the CI step passes no globs.

The test needs Node.js and a reachable package registry. When the pinned CLI
cannot run at all, it skips rather than fails, so an offline host does not
turn an unrelated change red.
"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

import pytest
from markdown_gate import makefile_version

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MAKEFILE = "Makefile"
CONFIG = ".markdownlint-cli2.jsonc"
IGNORED_DIRECTORY = "execplans"

#: A document the default rule set accepts.
_VALID = "# Title\n\nA clean paragraph.\n"

#: A document the default rule set rejects with MD009.
_INVALID = "# Title\n\nTrailing spaces   \n"


def _pinned_version() -> str:
    """Return the CLI version the Makefile pins, or fail with a clear message."""
    makefile = (REPOSITORY_ROOT / MAKEFILE).read_text(encoding="utf-8")
    version = makefile_version(makefile)
    assert version, "the Makefile must pin MARKDOWNLINT_CLI2_VERSION"
    return version


def _lint(directory: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    """Run the pinned CLI in ``directory`` with the given extra arguments."""
    if shutil.which("npx") is None:
        pytest.skip("npx is not installed, so the pinned CLI cannot run")
    command = [
        "npx",
        "--yes",
        f"markdownlint-cli2@{_pinned_version()}",
        *arguments,
    ]
    return subprocess.run(
        command,
        cwd=directory,
        capture_output=True,
        text=True,
        check=False,
    )


def _output(result: subprocess.CompletedProcess[str]) -> str:
    """Return the CLI's standard output and error as one report.

    The CLI splits its report: the banner, selected-file count and summary
    go to standard output, while each per-file finding goes to standard
    error. Both halves are evidence, so assertions read them together.
    """
    return result.stdout + result.stderr


def _require_cli(result: subprocess.CompletedProcess[str]) -> None:
    """Skip when the pinned CLI could not be downloaded or started."""
    output = _output(result)
    if "markdownlint-cli2 v" in output:
        return
    pytest.skip(f"the pinned Markdown lint CLI did not run:\n{output[-2000:]}")


def _linted(result: subprocess.CompletedProcess[str]) -> int:
    """Return the number of files the CLI reported linting."""
    match = re.search(r"Linting: (\d+) files?", result.stdout)
    assert match, result.stdout
    return int(match.group(1))


def _fixture(tmp_path: Path) -> Path:
    """Build a tree holding the repository configuration and its documents."""
    (tmp_path / CONFIG).write_text(
        (REPOSITORY_ROOT / CONFIG).read_text(encoding="utf-8"),
        encoding="utf-8",
    )
    (tmp_path / "valid.md").write_text(_VALID, encoding="utf-8")
    ignored = tmp_path / IGNORED_DIRECTORY
    ignored.mkdir()
    (ignored / "ignored.md").write_text(_INVALID, encoding="utf-8")
    return tmp_path


def test_a_valid_document_passes(tmp_path: Path) -> None:
    """The configuration's selection lints the clean document and no other."""
    result = _lint(_fixture(tmp_path))
    _require_cli(result)
    assert result.returncode == 0, _output(result)
    assert _linted(result) == 1, _output(result)
    assert "Summary: 0 issues in 0 files" in result.stdout, _output(result)


def test_an_invalid_document_fails_and_names_itself(tmp_path: Path) -> None:
    """A rejected document propagates its exit status and its own path."""
    fixture = _fixture(tmp_path)
    (fixture / "invalid.md").write_text(_INVALID, encoding="utf-8")
    result = _lint(fixture)
    _require_cli(result)
    output = _output(result)
    assert result.returncode != 0, output
    assert _linted(result) == 2, output
    assert "invalid.md" in output, output
    assert "MD009" in output, output


def test_an_ignored_document_is_never_linted(tmp_path: Path) -> None:
    """The same rejected content passes when it sits under an ignored tree."""
    result = _lint(_fixture(tmp_path))
    _require_cli(result)
    output = _output(result)
    assert result.returncode == 0, output
    assert "ignored.md" not in output, output
    assert _linted(result) == 1, output


def test_a_caller_supplied_glob_selects_the_same_files(tmp_path: Path) -> None:
    """Adding ``**/*.md`` on the command line changes neither set nor result."""
    fixture = _fixture(tmp_path)
    (fixture / "invalid.md").write_text(_INVALID, encoding="utf-8")
    configured = _lint(fixture)
    explicit = _lint(fixture, "**/*.md")
    _require_cli(configured)
    _require_cli(explicit)
    reported = _output(configured) + _output(explicit)
    assert configured.returncode == explicit.returncode != 0, reported
    assert _linted(configured) == _linted(explicit) == 2, reported
    assert "invalid.md" in _output(explicit), reported