"""Judge Makefile targets in the Markdown gate contract.

`make all` is documented as the local commit gate, so a target it omits, or
a recipe that names its own globs or ignores a lint failure, reopens the gap
the Markdown contract exists to close. These judgements are pure so the
contract test can drive each one against a mutated fixture.
"""

from __future__ import annotations

import re
from collections.abc import Iterable


def _target_declaration(makefile: str, target: str) -> str | None:
    """Return the line declaring a target, without its recipe."""
    prefix = f"{target}:"
    return next(
        (line for line in makefile.splitlines() if line.startswith(prefix)), None
    )


def prerequisites(makefile: str, target: str) -> list[str]:
    """Return the prerequisites named on a target's declaration line.

    Parameters
    ----------
    makefile : str
        The Makefile text.
    target : str
        The target whose declaration to read.

    Returns
    -------
    list[str]
        The whitespace-separated prerequisite names, or an empty list when
        the target is absent.
    """
    line = _target_declaration(makefile, target)
    if line is None:
        return []
    return line.split(":", 1)[1].split("#", 1)[0].split()


def prerequisite_faults(
    makefile: str, target: str, wanted: Iterable[str]
) -> list[str]:
    """Return each wanted prerequisite that a target does not name.

    Parameters
    ----------
    makefile : str
        The Makefile text.
    target : str
        The target whose prerequisites are required.
    wanted : collections.abc.Iterable[str]
        The prerequisite names that must be present.

    Returns
    -------
    list[str]
        One message per missing prerequisite; empty when every name is
        present.
    """
    declared = prerequisites(makefile, target)
    return [
        f"{target} is missing prerequisite {name}"
        for name in wanted
        if name not in declared
    ]


def recipe_lines(makefile: str, target: str) -> list[str]:
    """Return a target's recipe lines, with their leading tabs removed.

    Parameters
    ----------
    makefile : str
        The Makefile text.
    target : str
        The target whose recipe to read.

    Returns
    -------
    list[str]
        The recipe lines in order, or an empty list when the target is
        absent.
    """
    lines = makefile.splitlines()
    start = next(
        (
            index
            for index, line in enumerate(lines)
            if line.startswith(f"{target}:")
        ),
        None,
    )
    if start is None:
        return []
    recipe: list[str] = []
    for line in lines[start + 1 :]:
        if line.startswith("\t"):
            recipe.append(line[1:])
        elif not line.strip() or line.lstrip().startswith("#"):
            continue
        else:
            break
    return recipe


def _make_prefixes(head: str) -> str:
    """Return the run of Make recipe prefixes at the start of a command."""
    stop = next((i for i, char in enumerate(head) if char not in "@-+"), len(head))
    return head[:stop]


def mdlint_faults(makefile: str, target: str) -> list[str]:
    """Return what is wrong with a recipe's ``$(MDLINT)`` invocation.

    Parameters
    ----------
    makefile : str
        The Makefile text.
    target : str
        The target whose recipe to judge.

    Returns
    -------
    list[str]
        One message per fault; empty when the recipe passes no globs or
        exclusion arguments. Only ``--fix`` is allowed, on ``fmt``, and
        ``fmt`` must pass it: a recipe that merely runs the linter leaves
        the formatting fix undone. The invocation must not carry Make's
        ``-`` prefix, which ignores the command's exit status.
    """
    invocations = [
        line.strip()
        for line in recipe_lines(makefile, target)
        if "$(MDLINT)" in line
    ]
    if not invocations:
        return [f"{target} does not invoke $(MDLINT)"]
    allowed = ["--fix"] if target == "fmt" else []
    faults: list[str] = []
    fixes = False
    for line in invocations:
        head, _separator, tail = line.partition("$(MDLINT)")
        prefixes = _make_prefixes(head)
        if head[len(prefixes) :].strip():
            faults.append(
                f"{target} mentions $(MDLINT) without running it: {line!r}"
            )
            continue
        if "-" in prefixes:
            faults.append(
                f"{target} ignores a $(MDLINT) failure with the '-' prefix: {line!r}"
            )
            continue
        arguments = tail.split()
        fixes = fixes or "--fix" in arguments
        extra = [argument for argument in arguments if argument not in allowed]
        if extra:
            faults.append(
                f"{target} passes {extra!r} to $(MDLINT); "
                "selection must come from the shared configuration"
            )
    if target == "fmt" and not fixes:
        faults.append(f"{target} does not pass --fix to $(MDLINT)")
    return faults


def mdlint_runner_faults(makefile: str) -> list[str]:
    """Return what is wrong with the default ``MDLINT`` runner.

    Parameters
    ----------
    makefile : str
        The Makefile text.

    Returns
    -------
    list[str]
        One message per fault; empty only when ``MDLINT`` is exactly
        ``npx --yes markdownlint-cli2@$(MARKDOWNLINT_CLI2_VERSION)``. The
        exact form is the point: a looser check accepts a no-op such as
        ``true $(MARKDOWNLINT_CLI2_VERSION)`` or
        ``npx --yes echo markdownlint-cli2@...``, each of which mentions
        the pinned version without running it. A contributor who needs a
        different runner overrides ``MDLINT`` on the command line rather
        than editing the default.
    """
    match = re.search(r"^MDLINT\s*\?=\s*(.+)$", makefile, re.MULTILINE)
    if match is None:
        return ["Makefile does not declare MDLINT"]
    runner = match.group(1).split("#", 1)[0].strip()
    expected = [
        "npx",
        "--yes",
        "markdownlint-cli2@$(MARKDOWNLINT_CLI2_VERSION)",
    ]
    if runner.split() != expected:
        return [f"MDLINT must run {' '.join(expected)}: {runner!r}"]
    return []