"""Hold the `codescene` environment to the job that uploads coverage.

The environment's deployment policy admits `main` alone, so it is where
the CodeScene token is safe to hand out. Every job that calls the
uploader declares it, no other job does, and no workflow a pull request
can start declares it in any job: a declaration there would let branch
code ask for the token.
"""

from __future__ import annotations

import typing as typ

from coverage_shape_rules import UPLOADER_ACTION, steps_using
from workflow_reader import Workflow, pull_request_closure

#: The environment the uploading job runs in.
ENVIRONMENT: typ.Final = "codescene"

#: What each fault says, so the probes can name the one they expect.
MISSING: typ.Final = f"the uploading job must declare `environment: {ENVIRONMENT}`"
STRAY: typ.Final = f"declares `{ENVIRONMENT}` but uploads nothing"
REACHABLE: typ.Final = f"is reachable from a pull request and declares `{ENVIRONMENT}`"
NO_UPLOADER: typ.Final = "no workflow job calls the CodeScene uploader"


def environment_name(job: dict[str, typ.Any]) -> str | None:
    """Return the environment a job declares, from either accepted form.

    Returns
    -------
    str or None
        The environment's name, whether written as a string or as a
        mapping with a `name`, or None when the job declares none.

    Examples
    --------
    >>> environment_name({"environment": "codescene"})
    'codescene'
    >>> environment_name({"environment": {"name": "codescene", "url": "x"}})
    'codescene'
    >>> environment_name({}) is None
    True
    """
    match job.get("environment"):
        case str() as name:
            return name
        case {"name": str() as name}:
            return name
        case _:
            return None


def environment_faults(workflows: list[Workflow]) -> list[str]:
    r"""Return every departure from the `codescene` environment placement.

    Returns
    -------
    list[str]
        One message per fault, naming the workflow and job; empty when
        the placement holds.

    Examples
    --------
    >>> flow = Workflow.parse("p.yml", "on: push\njobs: {j: {steps: []}}\n")
    >>> environment_faults([flow])
    ['no workflow job calls the CodeScene uploader']
    """
    holders = {
        (flow.path, job) for flow, job, _ in steps_using(workflows, UPLOADER_ACTION)
    }
    if not holders:
        return [NO_UPLOADER]
    return (
        _missing(workflows, holders)
        + _stray(workflows, holders)
        + _reachable(pull_request_closure(workflows))
    )


def _declaring(workflows: list[Workflow]) -> list[tuple[str, str]]:
    """Return each job, as workflow path and job id, declaring the environment."""
    return [
        (flow.path, name)
        for flow in workflows
        for name, job in flow.jobs()
        if environment_name(job) == ENVIRONMENT
    ]


def _missing(workflows: list[Workflow], holders: set[tuple[str, str]]) -> list[str]:
    """Return the uploading jobs that do not declare the environment."""
    declared = set(_declaring(workflows))
    return [f"{path}:{job} {MISSING}" for path, job in sorted(holders - declared)]


def _stray(workflows: list[Workflow], holders: set[tuple[str, str]]) -> list[str]:
    """Return the jobs that declare the environment without uploading."""
    return [
        f"{path}:{job} {STRAY}"
        for path, job in _declaring(workflows)
        if (path, job) not in holders
    ]


def _reachable(closure: list[Workflow]) -> list[str]:
    """Return the pull-request-reachable jobs that declare the environment."""
    return [f"{path}:{job} {REACHABLE}" for path, job in _declaring(closure)]
