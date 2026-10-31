"""Judge Makefile targets in the Markdown gate contract.

`make all` is documented as the local commit gate, so a target it omits, or
a recipe that names its own globs or ignores a lint failure, reopens the gap
the Markdown contract exists to close. These judgements are pure so the
contract test can drive each one against a mutated fixture.
"""

from __future__ import annotations

import typing as typ


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
    makefile: str, target: str, wanted: typ.Iterable[str]
) -> list[str]:
    """Return each wanted prerequisite that a target does not name.

    Parameters
    ----------
    makefile : str
        The Makefile text.
    target : str
        The target whose prerequisites are required.
    wanted : typing.Iterable[str]
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
        exclusion arguments. Only ``--fix`` is allowed, on ``fmt``, and the
        invocation must not carry Make's ``-`` prefix, which ignores the
        command's exit status.
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
        extra = [argument for argument in tail.split() if argument not in allowed]
        if extra:
            faults.append(
                f"{target} passes {extra!r} to $(MDLINT); "
                "selection must come from the shared configuration"
            )
    return faults