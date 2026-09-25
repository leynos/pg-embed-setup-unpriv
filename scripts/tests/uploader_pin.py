"""Hold the CodeScene uploader to its approved pin and inputs.

At the approved pin the uploader treats its committed `cli-manifest.json`
as the trust anchor for the cs-coverage archive, and it rejects a
non-empty `installer-checksum` outright. So no workflow passes that
input, none references the `CODESCENE_CLI_SHA256` variable that fed it,
every uploader step is pinned to the one approved SHA, and the dispatch
workflow that refreshed the variable stays deleted.

Every rule reads the workflows as `workflow_reader` parses them, so a
commented-out step or input is not a step or an input, and the owner and
repository in a `uses:` value are matched without regard to case, as
GitHub resolves them.
"""

from __future__ import annotations

import typing as typ

from coverage_shape_rules import UPLOADER_ACTION, steps_using
from workflow_reader import Workflow, scalars

#: The one uploader revision a workflow may pin.
APPROVED_PIN: typ.Final = "a5765019912a8ab6882b12db049c7cde635f3a85"

#: The input the uploader rejects at the approved pin, and the variable
#: whose only consumer it was, both folded.
DEPRECATED: typ.Final = ("installer-checksum", "codescene_cli_sha256")

#: The dispatch workflow that refreshed the variable, folded.
REFRESH_WORKFLOW: typ.Final = "get-codescene-sha.yml"

#: What an empty reading says, so a test can name it.
NO_UPLOADER: typ.Final = "no workflow step calls the CodeScene uploader"


def uploader_pins(workflows: list[Workflow]) -> list[tuple[str, str]]:
    r"""Return each active uploader step's ref, with the workflow holding it.

    Returns
    -------
    list[tuple[str, str]]
        The workflow path and the text after the step's first `@`, which
        is empty when the step names no ref.

    Examples
    --------
    >>> text = "on: push\njobs: {j: {steps: [{uses: Leynos/Shared-Actions/"
    >>> text += ".github/actions/upload-codescene-coverage@v1}]}}\n"
    >>> uploader_pins([Workflow.parse("p.yml", text)])
    [('p.yml', 'v1')]
    """
    return [
        (flow.path, str(step["uses"]).partition("@")[2])
        for flow, _, step in steps_using(workflows, UPLOADER_ACTION)
    ]


def pin_faults(workflows: list[Workflow]) -> list[str]:
    r"""Return each uploader step pinned anywhere but the approved SHA.

    Returns
    -------
    list[str]
        One message per offending step, or one saying no uploader step
        was found, since an empty reading would pass over anything.

    Examples
    --------
    >>> pin_faults([Workflow.parse("p.yml", "on: push\njobs: {}\n")])
    ['no workflow step calls the CodeScene uploader']
    """
    pins = uploader_pins(workflows)
    if not pins:
        return [NO_UPLOADER]
    return [
        f"{path}: the uploader is pinned to {pin!r}, not {APPROVED_PIN}"
        for path, pin in pins
        if pin != APPROVED_PIN
    ]


def deprecated_mentions(workflows: list[Workflow], name: str) -> list[str]:
    r"""Return the workflows whose parsed keys or values mention `name`.

    Comments are not parsed, so a commented-out input is no mention.

    Returns
    -------
    list[str]
        The paths of the workflows mentioning `name`, case-folded.

    Examples
    --------
    >>> text = "on: push\n# installer-checksum: x\njobs: {}\n"
    >>> deprecated_mentions([Workflow.parse("p.yml", text)], "installer-checksum")
    []
    """
    return [
        flow.path
        for flow in workflows
        if any(name in text.lower() for text in scalars(flow.document))
    ]


def refresh_workflows(workflows: list[Workflow]) -> list[str]:
    r"""Return any workflow named as the deleted refresh dispatch.

    Returns
    -------
    list[str]
        The matching workflow paths, compared case-folded.

    Examples
    --------
    >>> flow = Workflow.parse(".github/workflows/Get-CodeScene-SHA.yml", "on: push\n")
    >>> refresh_workflows([flow])
    ['.github/workflows/Get-CodeScene-SHA.yml']
    """
    return [
        flow.path
        for flow in workflows
        if flow.path.lower().rsplit("/", 1)[-1] == REFRESH_WORKFLOW
    ]
