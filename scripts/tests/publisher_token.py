"""The rules for how the trunk publisher holds the CodeScene token.

The token stays out of every `env` mapping. The upload action is a
composite whose nested steps (artefact upload, cache) inherit the
environment of the step that calls it, so a token bound there reaches
steps that never need it. Instead one step, with an id and nothing else,
records whether the secret is present:

    echo "available=${{ secrets.CS_ACCESS_TOKEN != '' }}" >> "$GITHUB_OUTPUT"

The upload step runs on that output and the trunk ref, and takes the
token straight from the secret through its `access-token` input. Each
fault below is a way that shape can drift while the run stays green.
"""

from __future__ import annotations

import typing as typ

from step_conditions import conjuncts
from workflow_reader import Workflow, scalars

#: The token's name, folded because GitHub resolves secret names that way.
CODESCENE_TOKEN: typ.Final = "cs_access_token"

#: The token check's one command, folded and without whitespace.
CHECK_COMMAND: typ.Final = (
    'echo"available=${{secrets.cs_access_token!=\'\'}}">>"$github_output"'
)

#: The uploader's token input, folded and without whitespace.
TOKEN_INPUT: typ.Final = "${{secrets.cs_access_token}}"

#: The conjunct that confines an upload to the trunk.
TRUNK_CONJUNCT: typ.Final = "github.ref == 'refs/heads/main'"

Step = dict[str, typ.Any]


def token_faults(flow: Workflow, job: str, upload: Step) -> list[str]:
    """Return every way the publisher's token handling departs from the shape.

    Examples
    --------
    >>> flow = Workflow.parse("p.yml", "on: push\\njobs: {j: {steps: [{}]}}\\n")
    >>> token_faults(flow, "j", flow.document["jobs"]["j"]["steps"][0])[:1]
    ['runs no single token check before the upload step']
    """
    check = token_check(flow, job, upload)
    faults = (
        []
        if check is not None
        else ["runs no single token check before the upload step"]
    )
    faults += _check_faults(check) + _guard_faults(upload, check)
    if _folded(upload.get("with", {}).get("access-token", "")) != TOKEN_INPUT:
        faults.append("the uploader is not given the token from its secret")
    return faults + _scope_faults(flow, upload, check)


def token_check(flow: Workflow, job: str, upload: Step) -> Step | None:
    """Return the one step before the upload that records the token check."""
    steps = dict(flow.jobs())[job].get("steps") or []
    before = steps[: steps.index(upload)] if upload in steps else []
    checks = [
        step
        for step in before
        if isinstance(step, dict) and _folded(step.get("run", "")) == CHECK_COMMAND
    ]
    return checks[0] if len(checks) == 1 else None


def _check_faults(check: Step | None) -> list[str]:
    """Return why the token check could be skipped or read as absent."""
    if check is None:
        return []
    faults = [] if check.get("id") else ["the token check has no id"]
    if "if" in check:
        faults.append("the token check is conditional")
    if "env" in check:
        faults.append("the token check declares an env")
    return faults


def _guard_faults(upload: Step, check: Step | None) -> list[str]:
    """Return why the upload's condition does not require the check and the trunk.

    Both conjuncts are compared whole, and a bare `||` is refused: a
    disjunction anywhere makes every conjunct optional.
    """
    parts = conjuncts(upload.get("if", ""))
    if parts is None:
        return [f"condition {upload.get('if')!r} has a disjunction"]
    available = f"steps.{(check or {}).get('id')}.outputs.available == 'true'"
    if check is not None and TRUNK_CONJUNCT in parts and available in parts:
        return []
    return [
        f"condition {upload.get('if')!r} must require the token check and the trunk"
    ]


def _scope_faults(flow: Workflow, upload: Step, check: Step | None) -> list[str]:
    """Return where the token appears beyond the check and the upload input."""
    envs = [flow.document.get("env")] + [job.get("env") for _, job in flow.jobs()]
    envs += [step.get("env") for _, step in flow.steps()]
    faults = ["the token is bound in an env"] if any(map(_mentions, envs)) else []
    # Envs are judged above; everything else a step says is judged here, and
    # the upload may name the token only through its `access-token` input.
    outside = [
        {key: value for key, value in step.items() if key != "env"}
        for _, step in flow.steps()
        if step is not upload and step is not check
    ]
    inputs = {
        k: v for k, v in (upload.get("with") or {}).items() if k != "access-token"
    }
    outside.append(
        {
            **{k: v for k, v in upload.items() if k not in {"env", "with"}},
            "with": inputs,
        }
    )
    if any(map(_mentions, outside)):
        faults.append("another step references the token")
    return faults


def _mentions(node: object) -> bool:
    """Return whether any scalar in a parsed node names the token."""
    return any(CODESCENE_TOKEN in text.lower() for text in scalars(node))


def _folded(value: object) -> str:
    """Return a value as case-folded text with all whitespace removed."""
    return "".join(str(value).split()).lower()
