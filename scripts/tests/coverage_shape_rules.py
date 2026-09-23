"""The CV-005 coverage-shape rules, as judges returning their offenders.

Each rule is a function from the parsed workflows to a list of offence
descriptions, empty when the rule holds. Keeping them apart from the
tests lets one suite hold the repository's own workflows to every rule
and a second suite drive each rule with a constructed workflow carrying
the hazard it exists for, so a rule that could never fire is caught as
well as a workflow that breaks one.

The pull-request rules run over `pull_request_closure`, not over the
workflows whose own triggers name a pull request: a called workflow runs
under its caller's trigger, and a rule that stopped at the caller would
share one blind spot with every other rule written the same way.
"""

from __future__ import annotations

import typing as typ

from step_conditions import conjuncts, may_run
from workflow_reader import Workflow, pull_request_closure, scalars

#: The shared action that measures coverage, owner and repository folded
#: because only those are case-insensitive to GitHub.
COVERAGE_ACTION: typ.Final = "leynos/shared-actions/.github/actions/generate-coverage"

#: The shared action that talks to CodeScene.
UPLOADER_ACTION: typ.Final = (
    "leynos/shared-actions/.github/actions/upload-codescene-coverage"
)

#: The token no pull-request lane may reference, folded because GitHub
#: resolves secret names case-insensitively.
CODESCENE_TOKEN: typ.Final = "cs_access_token"

#: The service's host, which no pull-request lane may contact.
CODESCENE_HOST: typ.Final = "codescene.io"

#: The one conjunct that confines an upload to the trunk.
TRUNK_CONJUNCT: typ.Final = "github.ref == 'refs/heads/main'"

#: The one conjunct that requires the token, folded like the token name.
TOKEN_CONJUNCT: typ.Final = "env.cs_access_token != ''"

Found = list[tuple[Workflow, str, dict[str, typ.Any]]]


def coordinate(uses: object) -> str:
    """Return the action a `uses` value names, folded, without its ref.

    Examples
    --------
    >>> coordinate("Leynos/Shared-Actions/x@v1@beta")
    'leynos/shared-actions/x'
    """
    return str(uses).partition("@")[0].lower()


def steps_using(workflows: list[Workflow], action: str) -> Found:
    """Return every step invoking `action` exactly, with where it is."""
    return [
        (flow, job, step)
        for flow in workflows
        for job, step in flow.steps()
        if coordinate(step.get("uses", "")) == action
    ]


def step_input(step: dict[str, typ.Any], key: str) -> str:
    """Return one `with:` input, folded, or `<unset>` when absent."""
    return str((step.get("with") or {}).get(key, "<unset>")).lower()


def codescene_contacts(workflows: list[Workflow]) -> list[str]:
    """Return every way a pull-request lane could reach CodeScene.

    The token counts wherever it is referenced (a `run` body, an input,
    any `env` scope, a named `secrets:` forward), `secrets: inherit`
    counts because it forwards the token without naming it, and the host
    counts because a lane can call the service with neither the action
    nor the command.
    """
    lanes = pull_request_closure(workflows)
    found = [
        f"{flow.path}: {what}"
        for flow in lanes
        for text in scalars(flow.document)
        for what in _contact_in(text.lower())
    ]
    inherits = [
        f"{flow.path}:{job}: forwards every secret"
        for flow in lanes
        for job, mapping in flow.jobs()
        if str(mapping.get("secrets", "")).lower() == "inherit"
    ]
    actions = [
        f"{flow.path}:{job}: uses the uploader action"
        for flow, job, _ in steps_using(lanes, UPLOADER_ACTION)
    ]
    return found + inherits + actions


def _contact_in(text: str) -> list[str]:
    """Return which CodeScene contacts one folded scalar carries."""
    marks = {
        CODESCENE_TOKEN: "references the access token",
        CODESCENE_HOST: "names the CodeScene host",
        "cs-coverage": "runs the CodeScene CLI",
    }
    return [what for mark, what in marks.items() if mark in text]


def measuring_lanes(workflows: list[Workflow]) -> Found:
    """Return the pull-request coverage steps whose conditions let them run.

    A step discoverable by its action but guarded by `if: false`, or by a
    matrix value no leg carries, measures nothing, so presence alone is
    not enough.
    """
    lanes = pull_request_closure(workflows)
    jobs = {(flow.path, name): job for flow in lanes for name, job in flow.jobs()}
    return [
        (flow, name, step)
        for flow, name, step in steps_using(lanes, COVERAGE_ACTION)
        if may_run(jobs[flow.path, name], step)
    ]


def unratcheted_lanes(workflows: list[Workflow]) -> list[str]:
    """Return pull-request coverage steps that do not compare locally."""
    required = (("with-ratchet", "true"), ("publish-artefact", "false"))
    return [
        f"{flow.path}:{job}: {key} is {step_input(step, key)}"
        for flow, job, step in steps_using(
            pull_request_closure(workflows), COVERAGE_ACTION
        )
        for key, expected in required
        if step_input(step, key) != expected
    ]


def publisher_faults(workflows: list[Workflow]) -> list[str]:
    """Return what is wrong with the one trunk publisher, if anything."""
    publishers = steps_using(workflows, UPLOADER_ACTION)
    if len(publishers) != 1:
        where = [f"{flow.path}:{job}" for flow, job, _ in publishers]
        return [f"expected exactly one upload step, found {where}"]
    flow, job, step = publishers[0]
    faults = [
        *_trigger_faults(flow),
        *_guard_faults(step),
        *_token_scope_faults(flow, step),
        *_concurrency_faults(flow, job),
        *_generation_faults(flow, job, step),
    ]
    if step_input(step, "mode") != "upload":
        faults.append(f"uploads in {step_input(step, 'mode')} mode")
    return [
        *(f"{flow.path}:{job}: {fault}" for fault in faults),
        *_contacts_outside(workflows, step),
    ]


def _contacts_outside(workflows: list[Workflow], step: dict[str, typ.Any]) -> list[str]:
    """Return every CodeScene contact in any workflow but the upload step.

    A second publisher need not use the uploader action: a `cs-coverage`
    command, a call to the host or the token in any other workflow can
    race the one that does, so the contacts are read everywhere with the
    upload step itself left out.
    """
    return [
        f"{flow.path}: {what} outside the upload step"
        for flow in workflows
        for text in scalars(_without(flow.document, step))
        for what in _contact_in(text.lower())
    ]


def _without(node: object, target: object) -> object:
    """Return a copy of a parsed node with one sub-node, by identity, removed."""
    if node is target:
        return None
    if isinstance(node, dict):
        return {key: _without(value, target) for key, value in node.items()}
    if isinstance(node, list):
        return [_without(item, target) for item in node]
    return node


def _trigger_faults(flow: Workflow) -> list[str]:
    """Return why the publisher's triggers are not the trunk's."""
    faults = [] if flow.on_push_to_main_only else ["does not run on push to main"]
    if flow.on_pull_request:
        faults.append("also answers a pull request")
    return faults


def _guard_faults(step: dict[str, typ.Any]) -> list[str]:
    """Return why the upload step's condition does not confine it."""
    parts = conjuncts(step.get("if", ""))
    if parts is None:
        return [f"condition {step.get('if')!r} has a disjunction"]
    # Both conjuncts are compared whole: a guard naming the token in any
    # other way, `== ''` or `!env.CS_ACCESS_TOKEN`, runs the upload only
    # when there is nothing to upload with.
    has_token = any(part.lower() == TOKEN_CONJUNCT for part in parts)
    if TRUNK_CONJUNCT in parts and has_token:
        return []
    return [f"condition {step.get('if')!r} must require the trunk and the token"]


def _token_scope_faults(flow: Workflow, step: dict[str, typ.Any]) -> list[str]:
    """Return why the token is not held by the upload step alone.

    A token moved to a wider scope, or to a neighbouring step, leaves
    the upload step's own guard reading an empty value, so publishing
    stops without anything failing.
    """
    own = step.get("env") or {}
    faults = [] if _mentions_token(own) else ["upload step does not declare the token"]
    wider = [flow.document.get("env"), *(job.get("env") for _, job in flow.jobs())]
    if any(_mentions_token(scope) for scope in wider):
        faults.append("the token is declared at workflow or job scope")
    others = [other for _, other in flow.steps() if other is not step]
    if any(_mentions_token(other) for other in others):
        faults.append("another step references the token")
    return faults


def _mentions_token(node: object) -> bool:
    """Return whether any scalar in a parsed node names the token."""
    return any(CODESCENE_TOKEN in text.lower() for text in scalars(node))


def _concurrency_faults(flow: Workflow, job: str) -> list[str]:
    """Return why the publisher might run twice at once or be cut short.

    A group is required at workflow scope, and cancelling in progress is
    refused at either scope: a cancelled publisher abandons its upload
    and its ratchet baseline, where a queued one publishes later.
    """
    workflow_scope = flow.document.get("concurrency")
    job_scope = dict(flow.jobs()).get(job, {}).get("concurrency")
    faults = [] if _names_a_group(workflow_scope) else ["declares no concurrency group"]
    for scope in (workflow_scope, job_scope):
        if (
            isinstance(scope, dict)
            and scope.get("cancel-in-progress", False) is not False
        ):
            faults.append("cancels the publisher in progress")
    return faults


def _names_a_group(scope: object) -> bool:
    """Return whether a `concurrency` value names a non-empty group.

    Examples
    --------
    >>> _names_a_group("coverage"), _names_a_group({"cancel-in-progress": False})
    (True, False)
    """
    group = scope.get("group") if isinstance(scope, dict) else scope
    return isinstance(group, str) and bool(group.strip())


def _generation_faults(
    flow: Workflow, job: str, upload: dict[str, typ.Any]
) -> list[str]:
    """Return why the publisher might upload a report it never generated."""
    mapping = dict(flow.jobs())[job]
    before = (mapping.get("steps") or [])[: (mapping.get("steps") or []).index(upload)]
    generates = [
        step
        for step in before
        if isinstance(step, dict)
        and coordinate(step.get("uses", "")) == COVERAGE_ACTION
        and may_run({}, step)
    ]
    return [] if generates else ["generates no coverage before the upload step"]
