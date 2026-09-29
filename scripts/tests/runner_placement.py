"""The runner-placement judgement: where a lane runs, and for how long.

Ubicloud's cache proxy is scoped by ref, and a pull request from a fork
cannot obtain an Ubicloud runner at all. A lane that names an Ubicloud
runner therefore selects it by an expression that falls back to the
hosted pool for a fork, and states its own ceiling, because an Ubicloud
runner is a self-hosted just-in-time runner that GitHub's six-hour cap
for hosted jobs does not bound.

Examples
--------
>>> selected_runner(ESTATE_EXPRESSION, "fork")
'ubuntu-latest'
>>> selected_runner(ESTATE_EXPRESSION, "push")
'ubicloud-standard-2'
>>> len(placement_faults("ubuntu-latest"))
3
>>> placement_faults(ESTATE_EXPRESSION)
[]
"""

from __future__ import annotations

import re
import typing as typ

if typ.TYPE_CHECKING:
    from workflow_reader import Workflow

#: The Ubicloud runner class every placed lane names.
UBICLOUD_LABEL: typ.Final = "ubicloud-standard-2"

#: The hosted runner a fork's pull request falls back to.
HOSTED_LABEL: typ.Final = "ubuntu-latest"

#: The context value that is true only for a pull request from a fork.
FORK_CONDITION: typ.Final = "github.event.pull_request.head.repo.fork"

#: The estate's runner expression, as a lane writes it.
ESTATE_EXPRESSION: typ.Final = (
    "${{ github.event.pull_request.head.repo.fork && 'ubuntu-latest' "
    "|| 'ubicloud-standard-2' }}"
)

#: The estate shape: a condition, a quoted hosted arm and a quoted other arm.
_ESTATE_SHAPE: typ.Final = re.compile(
    r"\$\{\{\s*(?P<condition>[^&|]+?)\s*&&\s*'(?P<hosted>[^']*)'"
    r"\s*\|\|\s*'(?P<other>[^']*)'\s*\}\}"
)

#: The kinds of run a runner expression is evaluated for. A push and a
#: dispatch have no pull request, so the fork value is null and they
#: behave as a same-repository pull request does.
ORIGINS: typ.Final = ("push", "same-repository", "fork")


def selected_runner(runs_on: object, origin: str) -> str | None:
    """Return the label a `runs-on` value selects for a run.

    Returns None when the value is not the estate's
    `<fork> && '<hosted>' || '<label>'` shape. A literal label is not the
    shape: a lane that never falls back cannot serve a fork, and a lane
    that never leaves the hosted pool is not placed at all.

    Parameters
    ----------
    runs_on : object
        The job's `runs-on` value as parsed; anything but a string has no
        selection.
    origin : str
        The kind of run: `push` (also a dispatch), `same-repository` or
        `fork`.

    Returns
    -------
    str or None
        The selected label, or None when the value is not the estate shape.
    """
    shape = _ESTATE_SHAPE.fullmatch(runs_on.strip()) if isinstance(runs_on, str) else None
    if shape is None or shape["condition"] != FORK_CONDITION:
        return None
    return shape["hosted" if origin == "fork" else "other"]


def placement_faults(runs_on: object, label: str = UBICLOUD_LABEL) -> list[str]:
    """Return one entry per kind of run the expression places wrongly.

    Empty when a fork falls back to hosted and every other run is on
    the Ubicloud runner class `label`.

    Parameters
    ----------
    runs_on : object
        The job's `runs-on` value as parsed.
    label : str
        The Ubicloud runner class every non-fork run must select.

    Returns
    -------
    list[str]
        One description per kind of run placed wrongly.
    """
    wanted = {
        "push": label,
        "same-repository": label,
        "fork": HOSTED_LABEL,
    }
    chosen = {origin: selected_runner(runs_on, origin) for origin in ORIGINS}
    return [
        f"{origin} selects {chosen[origin]}, wanted {wanted[origin]}"
        for origin in ORIGINS
        if chosen[origin] != wanted[origin]
    ]


def placed_jobs(
    workflows: list[Workflow],
) -> list[tuple[str, str, object, object]]:
    """Return every job whose `runs-on` names Ubicloud.

    Each entry is the workflow path, the job, its `runs-on` value and the
    `timeout-minutes` it states (None when it states none). A matrix
    expression such as `${{ matrix.runner }}` is not a name, so it is
    not listed: the matrix values are covered where they are written.

    Parameters
    ----------
    workflows : list[Workflow]
        The parsed workflows to search.

    Returns
    -------
    list[tuple[str, str, object, object]]
        Path, job, `runs-on` value and stated ceiling, in file and job order.
    """
    return [
        (flow.path, job_id, job.get("runs-on"), job.get("timeout-minutes"))
        for flow in workflows
        for job_id, job in flow.jobs()
        if "ubicloud" in str(job.get("runs-on", ""))
    ]
