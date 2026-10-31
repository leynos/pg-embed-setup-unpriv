"""Judge the local Markdown gate against the CI step it mirrors.

`make all` is documented as the commit gate, so a check CI runs and the
Makefile omits is a failure that surfaces only after a push. The Markdown
lint step is the worst of those: it runs before spelling, the release
tooling tests, lint, Mermaid validation and the test suite, so a rejected
Markdown edit skips every later result and the pull request shows one
unrelated red row (pull request #223).

The judgements here are pure, so the contract test can drive each one
against a mutated fixture before applying it to the real files.
"""

from __future__ import annotations

import json
import re
import typing as typ

from workflow_reader import Workflow

#: The action whose input and revision the contract holds.
ACTION = "DavidAnson/markdownlint-cli2-action"

#: The approved action revision and the CLI version it bundles. A bump
#: changes both entries together, in one reviewed edit.
APPROVED_PAIRINGS: typ.Final[dict[str, str]] = {
    "21c1be1b93ad9ed58fa840aacc3f279cde2a72ff": "0.23.2",
}


def _strip_jsonc_comments(text: str) -> str:
    """Return JSONC text with comments removed and strings left intact.

    Parameters
    ----------
    text : str
        The JSONC document, which may contain ``//`` and ``/* */`` comments.

    Returns
    -------
    str
        The same document as plain JSON.
    """
    stripped: list[str] = []
    index = 0
    in_string = False
    while index < len(text):
        character = text[index]
        if in_string:
            stripped.append(character)
            if character == "\\" and index + 1 < len(text):
                index += 1
                stripped.append(text[index])
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
            stripped.append(character)
        elif text.startswith("//", index):
            newline = text.find("\n", index)
            index = len(text) if newline == -1 else newline
            continue
        elif text.startswith("/*", index):
            end = text.find("*/", index + 2)
            index = len(text) if end == -1 else end + 2
            continue
        else:
            stripped.append(character)
        index += 1
    return "".join(stripped)


def config_faults(text: str) -> list[str]:
    """Return what is wrong with a configuration's file selection.

    Parameters
    ----------
    text : str
        The JSONC configuration document.

    Returns
    -------
    list[str]
        One message per fault; empty when the top-level ``globs`` array is
        non-empty and selects every Markdown file.
    """
    globs = json.loads(_strip_jsonc_comments(text)).get("globs")
    if not isinstance(globs, list) or not globs:
        return [f"globs must be a non-empty array, got {globs!r}"]
    if "**/*.md" not in globs:
        return [f"globs must include **/*.md, got {globs!r}"]
    return []


def _markdownlint_steps(workflow: Workflow) -> list[dict[str, typ.Any]]:
    """Return every step invoking the Markdown lint action."""
    return [
        step
        for _job, step in workflow.steps()
        if str(step.get("uses", "")).split("@", 1)[0] == ACTION
    ]


def _action_revision(step: dict[str, typ.Any]) -> str:
    """Return the revision after the ``@`` in a step's ``uses`` value."""
    uses = str(step.get("uses", ""))
    _owner, separator, revision = uses.partition("@")
    return revision if separator else ""


def glob_faults(workflow: Workflow) -> list[str]:
    """Return what is wrong with the action step's ``globs`` input.

    Parameters
    ----------
    workflow : Workflow
        The parsed workflow to judge.

    Returns
    -------
    list[str]
        One message per fault; empty when the action passes no globs.
    """
    steps = _markdownlint_steps(workflow)
    if not steps:
        return [f"no {ACTION} step"]
    faults: list[str] = []
    for step in steps:
        inputs = step.get("with")
        globs = inputs.get("globs") if isinstance(inputs, dict) else None
        if globs != "":
            faults.append(f"{ACTION} passes globs {globs!r}; it must pass none")
    return faults


def pairing_faults(revision: str, version: str) -> list[str]:
    """Return what is wrong with an action revision and a CLI version.

    Parameters
    ----------
    revision : str
        The action commit SHA.
    version : str
        The ``MARKDOWNLINT_CLI2_VERSION`` value.

    Returns
    -------
    list[str]
        One message per fault; empty when the pair is approved.
    """
    approved = APPROVED_PAIRINGS.get(revision)
    if approved is None:
        return [f"action revision {revision!r} is not approved"]
    if approved != version:
        return [
            f"action revision {revision!r} bundles {approved!r}, "
            f"Makefile pins {version!r}"
        ]
    return []


def makefile_version(makefile: str) -> str:
    """Return the Makefile's pinned Markdown lint CLI version."""
    match = re.search(
        r"^MARKDOWNLINT_CLI2_VERSION\s*\?=\s*([^\s#]+)", makefile, re.MULTILINE
    )
    return match.group(1) if match else ""


def pairing_faults_for(workflow: Workflow, makefile: str) -> list[str]:
    """Return the pairing faults across a workflow and the Makefile."""
    steps = _markdownlint_steps(workflow)
    if not steps:
        return [f"no {ACTION} step"]
    version = makefile_version(makefile)
    if not version:
        return ["Makefile does not declare MARKDOWNLINT_CLI2_VERSION"]
    return [
        fault
        for step in steps
        for fault in pairing_faults(_action_revision(step), version)
    ]


def _target_declaration(makefile: str, target: str) -> str | None:
    """Return the line declaring a target, without its recipe."""
    prefix = f"{target}:"
    return next(
        (line for line in makefile.splitlines() if line.startswith(prefix)), None
    )


def prerequisites(makefile: str, target: str) -> list[str]:
    """Return the prerequisites named on a target's declaration line."""
    line = _target_declaration(makefile, target)
    if line is None:
        return []
    return line.split(":", 1)[1].split("#", 1)[0].split()


def prerequisite_faults(
    makefile: str, target: str, wanted: typ.Iterable[str]
) -> list[str]:
    """Return each wanted prerequisite that a target does not name."""
    declared = prerequisites(makefile, target)
    return [
        f"{target} is missing prerequisite {name}"
        for name in wanted
        if name not in declared
    ]


def recipe_lines(makefile: str, target: str) -> list[str]:
    """Return a target's recipe lines, with their leading tabs removed."""
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
        exclusion arguments. Only ``--fix`` is allowed, on ``fmt``.
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
        _head, _separator, tail = line.partition("$(MDLINT)")
        extra = [argument for argument in tail.split() if argument not in allowed]
        if extra:
            faults.append(
                f"{target} passes {extra!r} to $(MDLINT); "
                "selection must come from the shared configuration"
            )
    return faults