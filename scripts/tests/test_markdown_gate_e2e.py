"""Run the Markdown gate against fixtures, through every path CI relies on.

The contract tests judge the Makefile, workflow and configuration as text,
so they can pass while the command they describe still rejects the wrong
files. These tests execute the pinned CLI and the Makefile recipe in a
temporary tree holding a copy of the repository's configuration, and assert
the behaviour the contract assumes:

- clean documents pass, and a document under an ignored directory is never
  linted even when it carries a violation;
- a rejected document fails and names itself;
- ``make markdownlint``, the bare pinned CLI the CI action runs, and the CLI
  with an explicit ``**/*.md`` all flag exactly the same files.

Every selectable document in the comparison tree carries a violation, so the
flagged set is the selected set: a path that drops a document, or lints an
ignored one, changes the set rather than only the count. The tests need Node.js, ``make`` and a
reachable package registry; when either tool is missing they skip on a
development host but fail in CI, where the gate must not go quiet.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import typing as typ
from pathlib import Path

import pytest
from makefile_gate import prerequisites, recipe_lines
from markdown_gate import makefile_version

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MAKEFILE = "Makefile"
CONFIG = ".markdownlint-cli2.jsonc"
IGNORED_DIRECTORY = "execplans"

#: A document the default rule set accepts.
_VALID = "# Title\n\nA clean paragraph.\n"

#: A document the default rule set rejects with MD009.
_INVALID = "# Title\n\nTrailing spaces   \n"

#: Every selectable document in the violating tree carries a violation, so
#: the flagged set is the selected set and a path cannot silently drop one.
_FLAGGED = {"alpha.md", "deep/gamma.md", "nested/beta.md"}

#: A finding line names the flagged file before its position.
_FINDING = re.compile(r"^(\S+\.md):\d+:\d+ error ", re.MULTILINE)


def _makefile_text() -> str:
    """Return the repository's Makefile text."""
    return (REPOSITORY_ROOT / MAKEFILE).read_text(encoding="utf-8")


def _pinned_version() -> str:
    """Return the CLI version the Makefile pins, or fail with a clear message."""
    version = makefile_version(_makefile_text())
    assert version, "the Makefile must pin MARKDOWNLINT_CLI2_VERSION"
    return version


def _unavailable(reason: str) -> typ.NoReturn:
    """Fail in CI when a required tool is missing; skip on a development host."""
    if os.environ.get("CI"):
        pytest.fail(f"the Markdown gate test requires {reason} in CI")
    pytest.skip(f"the Markdown gate test requires {reason}")


def _output(result: subprocess.CompletedProcess[str]) -> str:
    """Return the CLI's standard output and error as one report.

    The CLI splits its report: the banner, selected-file count and summary
    go to standard output, while each per-file finding goes to standard
    error. Both halves are evidence, so assertions read them together.
    """
    return result.stdout + result.stderr


def _require_cli(result: subprocess.CompletedProcess[str]) -> None:
    """Fail in CI, or skip locally, when the pinned CLI never started."""
    if "markdownlint-cli2 v" in _output(result):
        return
    _unavailable(f"a working pinned CLI:\n{_output(result)[-2000:]}")


def _lint(directory: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    """Run the pinned CLI in ``directory`` with the given extra arguments."""
    if shutil.which("npx") is None:
        _unavailable("npx")
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


def _make(directory: Path, target: str) -> subprocess.CompletedProcess[str]:
    """Run one target from the fixture Makefile in ``directory``.

    ``MDLINT`` is removed from the environment so the fixture's ``?=``
    definition, copied from the repository, is the one that runs.
    """
    if shutil.which("make") is None:
        _unavailable("make")
    environment = {key: value for key, value in os.environ.items() if key != "MDLINT"}
    return subprocess.run(
        ["make", "--no-print-directory", target],
        cwd=directory,
        capture_output=True,
        text=True,
        check=False,
        env=environment,
    )


def _flagged(result: subprocess.CompletedProcess[str]) -> set[str]:
    """Return the files the CLI reported a finding against."""
    return set(_FINDING.findall(_output(result)))


def _linted(result: subprocess.CompletedProcess[str]) -> int:
    """Return the number of files the CLI reported linting."""
    match = re.search(r"Linting: (\d+) files?", result.stdout)
    assert match, result.stdout
    return int(match.group(1))


def _configured_tree(tmp_path: Path) -> Path:
    """Write the repository configuration into an empty fixture tree."""
    (tmp_path / CONFIG).write_text(
        (REPOSITORY_ROOT / CONFIG).read_text(encoding="utf-8"),
        encoding="utf-8",
    )
    return tmp_path


def _violating_tree(tmp_path: Path) -> Path:
    """Build a tree where every selected document carries a violation.

    A document outside the selection carries one too, so a run that ignores
    the configuration's ``ignores`` is caught as well.
    """
    tree = _configured_tree(tmp_path)
    (tree / "alpha.md").write_text(_INVALID, encoding="utf-8")
    nested = tree / "nested"
    nested.mkdir()
    (nested / "beta.md").write_text(_INVALID, encoding="utf-8")
    deep = tree / "deep"
    deep.mkdir()
    (deep / "gamma.md").write_text(_INVALID, encoding="utf-8")
    ignored = tree / IGNORED_DIRECTORY
    ignored.mkdir()
    (ignored / "delta.md").write_text(_INVALID, encoding="utf-8")
    return tree


def _fixture_makefile() -> str:
    """Return the repository's MDLINT definitions and lint target, verbatim.

    The real declaration, its declared prerequisites and the real recipe are
    reproduced so ``make markdownlint`` traverses the same dependency edge
    as the repository target. The spelling gate needs the repository's git
    history and network toolchain, so its recipe is stubbed; its declaration
    edge is kept, which is what the contract test checks.
    """
    makefile = _makefile_text()
    runner = re.search(r"^MDLINT\s*\?=\s*(.+)$", makefile, re.MULTILINE)
    recipe = recipe_lines(makefile, "markdownlint")
    assert runner, "the Makefile must declare MDLINT"
    assert recipe, "markdownlint must have a recipe"
    declaration = " ".join(
        ["markdownlint:", *prerequisites(makefile, "markdownlint")]
    )
    definition = runner.group(1).split("#", 1)[0].strip()
    return (
        f"MARKDOWNLINT_CLI2_VERSION ?= {_pinned_version()}\n"
        f"MDLINT ?= {definition}\n"
        f"\n{declaration}\n"
        + "".join(f"\t{line}\n" for line in recipe)
        + "\nspelling:\n\t@:\n"
    )


def test_clean_documents_pass_and_ignored_trees_are_never_linted(
    tmp_path: Path,
) -> None:
    """The ignored document carries a violation but no finding names it."""
    tree = _configured_tree(tmp_path)
    (tree / "clean.md").write_text(_VALID, encoding="utf-8")
    ignored = tree / IGNORED_DIRECTORY
    ignored.mkdir()
    (ignored / "gamma.md").write_text(_INVALID, encoding="utf-8")
    result = _lint(tree)
    _require_cli(result)
    assert result.returncode == 0, _output(result)
    assert _flagged(result) == set(), _output(result)
    assert _linted(result) == 1, _output(result)


def test_an_invalid_document_fails_and_names_itself(tmp_path: Path) -> None:
    """Every selected violation propagates; the ignored one stays silent."""
    result = _lint(_violating_tree(tmp_path))
    _require_cli(result)
    assert result.returncode != 0, _output(result)
    assert _flagged(result) == _FLAGGED, _output(result)
    assert "MD009" in _output(result), _output(result)


def test_the_makefile_recipe_selects_the_same_files(tmp_path: Path) -> None:
    """``make markdownlint`` flags exactly the documents the CI path flags."""
    tree = _violating_tree(tmp_path)
    (tree / MAKEFILE).write_text(_fixture_makefile(), encoding="utf-8")
    configured = _lint(tree)
    via_make = _make(tree, "markdownlint")
    _require_cli(configured)
    _require_cli(via_make)
    assert _flagged(via_make) == _flagged(configured) == _FLAGGED, _output(via_make)
    # make reports its own failure status (2), so only the fact of failure
    # is comparable across the two paths.
    assert via_make.returncode != 0, _output(via_make)
    assert configured.returncode != 0, _output(configured)


def test_an_explicit_glob_selects_the_same_files(tmp_path: Path) -> None:
    """Adding ``**/*.md`` on the command line changes neither set nor result."""
    tree = _violating_tree(tmp_path)
    configured = _lint(tree)
    explicit = _lint(tree, "**/*.md")
    _require_cli(configured)
    _require_cli(explicit)
    assert _flagged(explicit) == _flagged(configured) == _FLAGGED, _output(explicit)
    assert configured.returncode == explicit.returncode != 0, _output(explicit)