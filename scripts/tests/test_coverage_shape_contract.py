"""Contracts on the coverage shape CV-005 defines.

The rule has two halves and they fail in opposite ways. A pull-request
lane that still calls CodeScene is red whenever the service is, for
reasons no commit here can fix; a trunk publisher that stops uploading
is silently green while the ratchet everyone compares against goes
stale. So the pull-request half is asserted as an absence and the trunk
half as a presence, and each absence is paired with a presence so that
deleting the lane cannot satisfy it.

Two parsing hazards are handled explicitly rather than assumed away.
YAML 1.1 reads a bare `on:` key as the boolean `True`, so both spellings
are looked up; a reader that took only the string would quantify over an
empty trigger set and pass over every workflow at once. And a `uses:`
value is a coordinate and a ref joined by the first `@`, while the ref
may contain one, so coordinates are split on the first.
"""

from __future__ import annotations

import typing as typ
from pathlib import Path

import pytest
import yaml

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_DIRECTORY = REPOSITORY_ROOT / ".github" / "workflows"

#: The shared action that measures coverage, owner and repository folded
#: because only those are case-insensitive to GitHub.
COVERAGE_ACTION: typ.Final = (
    "leynos/shared-actions/.github/actions/generate-coverage"
)

#: The shared action that talks to CodeScene.
UPLOADER_ACTION: typ.Final = (
    "leynos/shared-actions/.github/actions/upload-codescene-coverage"
)

#: The events that make a workflow a pull-request lane.
PULL_REQUEST_EVENTS: typ.Final = ("pull_request", "pull_request_target")

#: The token no pull-request lane may carry.
CODESCENE_TOKEN: typ.Final = "CS_ACCESS_TOKEN"


class Workflow(typ.NamedTuple):
    """One parsed workflow and what it needs to be judged.

    Attributes
    ----------
    name : str
        The file name, for failure messages.
    document : dict
        The parsed document.
    triggers : frozenset[str]
        The event names the workflow answers.
    """

    name: str
    document: dict[str, typ.Any]
    triggers: frozenset[str]

    @property
    def on_pull_request(self) -> bool:
        """Whether the workflow runs on a pull request.

        Returns
        -------
        bool
            True when it answers either pull-request event.
        """
        return any(event in self.triggers for event in PULL_REQUEST_EVENTS)

    @property
    def on_push_to_main(self) -> bool:
        """Whether the workflow runs on a push to the trunk.

        Returns
        -------
        bool
            True when a `push` trigger names `main`, or names no branch
            at all and so answers every push.
        """
        if "push" not in self.triggers:
            return False
        push = _triggers(self.document).get("push")
        if not isinstance(push, dict):
            return True
        branches = push.get("branches")
        return branches is None or "main" in list(branches)

    def steps(self) -> list[tuple[str, dict[str, typ.Any]]]:
        """Return every step of every job, with its job name.

        Returns
        -------
        list[tuple[str, dict]]
            The job's identifier and one step mapping, per step.
        """
        jobs = self.document.get("jobs")
        pairs = jobs.items() if isinstance(jobs, dict) else ()
        return [
            (str(name), step)
            for name, job in pairs
            for step in _step_list(job)
            if isinstance(step, dict)
        ]


def _step_list(job: object) -> list[object]:
    """Return one job's steps, or nothing when it declares none.

    Parameters
    ----------
    job : object
        The parsed job, whatever the YAML parser produced.

    Returns
    -------
    list[object]
        The steps as parsed.
    """
    if not isinstance(job, dict):
        return []
    steps = job.get("steps")
    return list(steps) if isinstance(steps, list) else []


def _steps_using(
    workflows: list[Workflow], action: str, *, on_pull_request: bool | None = None
) -> list[tuple[Workflow, str, dict[str, typ.Any]]]:
    """Return every step invoking `action`, with where it was found.

    Parameters
    ----------
    workflows : list[Workflow]
        The parsed workflows.
    action : str
        The folded action coordinate to match, by equality rather than
        containment: a neighbour whose path merely starts with this one
        is a different action.
    on_pull_request : bool or None
        Restrict to pull-request lanes, to everything else, or to
        neither when None.

    Returns
    -------
    list[tuple[Workflow, str, dict]]
        The workflow, the job's identifier and the step.
    """
    return [
        (flow, job, step)
        for flow in workflows
        if on_pull_request is None or flow.on_pull_request == on_pull_request
        for job, step in flow.steps()
        if _coordinate(step.get("uses", "")) == action
    ]


def _shell_lines(workflows: list[Workflow]) -> list[tuple[str, str, str]]:
    """Return every shell line of every step, with where it was found.

    Parameters
    ----------
    workflows : list[Workflow]
        The parsed workflows.

    Returns
    -------
    list[tuple[str, str, str]]
        The workflow's name, the job's identifier and one line.
    """
    return [
        (flow.name, job, line)
        for flow in workflows
        if flow.on_pull_request
        for job, step in flow.steps()
        for line in str(step.get("run", "")).splitlines()
    ]


def _input(step: dict[str, typ.Any], key: str) -> str:
    """Return one `with:` input, folded, or a marker when it is unset.

    Parameters
    ----------
    step : dict
        The parsed step.
    key : str
        The input's name.

    Returns
    -------
    str
        The value in lower case, or `<unset>`. A marker rather than an
        empty string, so a failure message distinguishes an absent input
        from one set to nothing.
    """
    return str((step.get("with") or {}).get(key, "<unset>")).lower()


def _guards_the_trunk(step: dict[str, typ.Any]) -> bool:
    """Return whether a step requires both the trunk ref and the token.

    Parameters
    ----------
    step : dict
        The parsed step.

    Returns
    -------
    bool
        True when its condition names both.
    """
    guard = str(step.get("if", ""))
    return "refs/heads/main" in guard and CODESCENE_TOKEN in guard


def _triggers(document: dict[str, typ.Any]) -> dict[str, typ.Any]:
    """Return a workflow's trigger mapping, whichever key YAML produced.

    Parameters
    ----------
    document : dict
        The parsed workflow.

    Returns
    -------
    dict
        Event name to its configuration. A trigger list such as
        `on: [push]` becomes a mapping to None, so callers can read
        membership and configuration through one shape.
    """
    raw = document.get("on", document.get(True))
    if isinstance(raw, dict):
        return raw
    if isinstance(raw, str):
        return {raw: None}
    if isinstance(raw, list):
        return {str(event): None for event in raw}
    return {}


def _coordinate(uses: object) -> str:
    """Return the action a `uses` value names, without its ref.

    Parameters
    ----------
    uses : object
        The step's `uses` value.

    Returns
    -------
    str
        The coordinate, folded to lower case.
    """
    return str(uses).partition("@")[0].lower()


def _workflows() -> list[Workflow]:
    """Return every workflow under `.github/workflows`.

    Returns
    -------
    list[Workflow]
        One entry per parsed file.

    Raises
    ------
    AssertionError
        If the directory holds no workflow, which would make every
        assertion here vacuous.
    """
    found: list[Workflow] = []
    for path in sorted(WORKFLOW_DIRECTORY.glob("*.y*ml")):
        document = yaml.safe_load(path.read_text(encoding="utf-8"))
        if not isinstance(document, dict):
            continue
        found.append(
            Workflow(path.name, document, frozenset(_triggers(document)))
        )
    assert found, f"no workflow parsed under {WORKFLOW_DIRECTORY}"
    return found


@pytest.fixture(scope="module")
def workflows() -> list[Workflow]:
    """Return the parsed workflows once for the module.

    Returns
    -------
    list[Workflow]
        Every workflow in the repository.
    """
    return _workflows()


def test_the_triggers_are_read_whichever_key_yaml_produced(
    workflows: list[Workflow],
) -> None:
    """Every workflow declares at least one trigger.

    This is the presence half that keeps the absence assertions below
    honest. YAML 1.1 reads a bare `on:` as the boolean `True`, so a
    reader looking only for the string finds no triggers anywhere,
    classifies nothing as a pull-request lane, and passes over a
    repository that violates every rule here.
    """
    silent = [flow.name for flow in workflows if not flow.triggers]
    assert not silent, (
        f"these workflows appear to declare no trigger, which means the "
        f"reader is wrong rather than the workflows: {silent}"
    )


def test_a_pull_request_lane_measures_coverage(
    workflows: list[Workflow],
) -> None:
    """Some pull-request lane invokes the coverage action.

    Paired with the absences below: "no pull-request lane calls
    CodeScene" is satisfied by a repository with no pull-request
    coverage at all, and that is the state this rule exists to avoid.
    """
    measuring = _steps_using(workflows, COVERAGE_ACTION, on_pull_request=True)
    assert measuring, (
        "no pull-request workflow invokes the coverage action; the ratchet "
        "has nothing to compare"
    )


def test_no_pull_request_lane_reaches_codescene(
    workflows: list[Workflow],
) -> None:
    """A pull-request lane compares locally and calls nothing.

    Three ways in, all closed. The uploader action, a `cs-coverage`
    command in a shell step, and the access token in any environment
    mapping: the token is included because a lane carrying it is a lane
    that intends to call, and because the step conditions elsewhere in
    these files are written in terms of it.
    """
    tokens = [
        f"{flow.name}: mentions {CODESCENE_TOKEN}"
        for flow in workflows
        if flow.on_pull_request
        and CODESCENE_TOKEN in yaml.safe_dump(flow.document)
    ]
    uploads = [
        f"{flow.name}:{job}: uses the uploader action"
        for flow, job, _ in _steps_using(
            workflows, UPLOADER_ACTION, on_pull_request=True
        )
    ]
    commands = [
        f"{name}:{job}: runs {line.strip()!r}"
        for name, job, line in _shell_lines(workflows)
        if "cs-coverage" in line
    ]
    offenders = tokens + uploads + commands
    assert not offenders, (
        f"a pull-request lane must not call CodeScene; the service's gate "
        f"configuration is not this repository's to fix: {offenders}"
    )


def test_every_pull_request_coverage_step_ratchets_and_publishes_nothing(
    workflows: list[Workflow],
) -> None:
    """The pull-request lane compares and keeps its report.

    Both inputs are asserted, because they answer different questions.
    Without the ratchet the lane measures and compares against nothing;
    with the artefact published it duplicates what the trunk lane
    uploads, under a name a later job may pick up instead.
    """
    required = (("with-ratchet", "true"), ("publish-artefact", "false"))
    wrong = [
        f"{flow.name}:{job}: {key} is {_input(step, key)}"
        for flow, job, step in _steps_using(
            workflows, COVERAGE_ACTION, on_pull_request=True
        )
        for key, expected in required
        if _input(step, key) != expected
    ]
    assert not wrong, f"pull-request coverage steps must ratchet locally: {wrong}"


def test_exactly_one_workflow_publishes_to_codescene(
    workflows: list[Workflow],
) -> None:
    """One publisher, on the trunk, uploading rather than checking.

    The count matters as much as the mode. Two publishers race to write
    the ratchet baseline every pull request then compares against, and
    the loser's report is the one that survives.
    """
    publishers = _steps_using(workflows, UPLOADER_ACTION)
    assert len(publishers) == 1, (
        f"expected exactly one CodeScene upload step; found "
        f"{[(flow.name, job) for flow, job, _ in publishers]}"
    )
    flow, job, step = publishers[0]
    assert flow.on_push_to_main, (
        f"{flow.name}:{job} uploads but does not run on a push to main"
    )
    mode = _input(step, "mode")
    assert mode == "upload", (
        f"{flow.name}:{job} uploads in {mode} mode; `check` is what CV-005 "
        f"removes from the pull-request lane and it belongs nowhere"
    )


def test_the_publisher_is_guarded_on_the_trunk_ref_and_the_token(
    workflows: list[Workflow],
) -> None:
    """The upload step names the ref as well as the token.

    The publisher also answers `workflow_dispatch`, which can be run
    from any branch. Guarded on the token alone, a dispatch from a
    feature branch uploads that branch's coverage and CodeScene records
    it as the trunk's, which then becomes the baseline every pull
    request is measured against.
    """
    unguarded = [
        f"{flow.name}:{job}: {step.get('if', '<none>')!r}"
        for flow, job, step in _steps_using(workflows, UPLOADER_ACTION)
        if not _guards_the_trunk(step)
    ]
    assert not unguarded, (
        f"an upload step must require both the trunk ref and the token: "
        f"{unguarded}"
    )


def test_the_publisher_serialises_itself(workflows: list[Workflow]) -> None:
    """The publishing workflow declares a concurrency group.

    Two pushes landing close together would otherwise upload for the
    same project and write the same ratchet baseline at once. Only the
    presence of a group is asserted; whether it cancels in progress runs
    is a judgement recorded in the workflow's own comment.
    """
    serial = [
        flow.name
        for flow, _, _ in _steps_using(workflows, UPLOADER_ACTION)
        if not flow.document.get("concurrency")
    ]
    assert not serial, (
        f"these workflows publish coverage and declare no concurrency group: "
        f"{serial}"
    )
