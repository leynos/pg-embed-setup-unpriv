"""Reads every coverage-invoking job out of the workflow files.

Separated from ``timeout_budgets`` so the workflow reading and the
nextest arithmetic stay legible apart, and so neither module outgrows
the 400-line limit ``AGENTS.md`` sets.
"""

import collections.abc as cabc
import pathlib
import typing as typ
from fractions import Fraction

import yaml
from timeout_budgets import (
    COVERAGE_ACTION,
    WATCHDOG_VARIABLE,
    WORKFLOWS_DIRECTORY,
    mapping_or_empty,
    sequence_or_empty,
)

#: One workflow file's name mapped to its parsed document.
WorkflowDocuments: typ.TypeAlias = cabc.Mapping[str, dict[str, object]]


class WorkflowReadError(RuntimeError):
    """Raised when a workflow file cannot be read or parsed.

    The lane query is pure and takes documents. Reading them is the one
    fallible step, so it reports its own failure rather than letting a
    parser's exception surface from what reads like a query.
    """


class CoverageJob(typ.NamedTuple):
    """One job that invokes the coverage action, with its budgets.

    Attributes
    ----------
    workflow : str
        The workflow file's name.
    job : str
        The job's identifier.
    steps : int
        How many coverage steps the job runs. Each gets its own watchdog,
        so the job must contain all of their budgets.
    watchdogs : tuple[Fraction | None, ...]
        The watchdog budget in force for each of those steps, in order,
        with None where neither the step nor the job sets one.
    job_timeout : Fraction or None
        The job's ``timeout-minutes`` in seconds, or None when it
        declares none and so inherits GitHub's six-hour default.
    conditions : tuple[tuple[object, object], ...]
        The ``if`` on each coverage step and on its job, in step order.
        A skipped step runs no ``cargo``, so its watchdog never arms and
        the tiers say nothing about it; the condition is part of what
        identifies a lane rather than incidental to it.
    """

    workflow: str
    job: str
    steps: int
    watchdogs: tuple[Fraction | None, ...]
    job_timeout: Fraction | None
    conditions: tuple[tuple[object, object], ...] = ()

    def __str__(self) -> str:
        """Return a location suitable for a failure message.

        Returns
        -------
        str
            ``workflow:job`` for this job.
        """
        return f"{self.workflow}:{self.job}"


def _watchdog_of(
    document: dict[str, object],
    job: dict[str, object],
    step: dict[str, object],
) -> Fraction | None:
    """Return the watchdog budget in force for one step.

    All three levels are read, innermost first, as GitHub resolves them.
    Both workflows here set the value at job level, so a contract
    reading only the step would find nothing and report every lane as
    inheriting the action's default, which is exactly backwards.

    Parameters
    ----------
    document : dict[str, object]
        The whole workflow document.
    job : dict[str, object]
        The enclosing job.
    step : dict[str, object]
        The coverage step.

    Returns
    -------
    Fraction or None
        The budget in seconds, exactly, or None when no level sets one.
        Read as a ``Fraction`` rather than a ``float`` because the
        ordering contract compares it against sums of nextest budgets
        that are themselves exact, and one ``float`` in a comparison
        converts the whole of it back.
    """
    for owner in (step, job, document):
        raw = mapping_or_empty(owner.get("env")).get(WATCHDOG_VARIABLE)
        if raw is not None:
            return Fraction(str(raw))
    return None


def load_workflow_documents(
    directory: pathlib.Path | None = None,
) -> dict[str, dict[str, object]]:
    """Read and parse every workflow document, keyed by file name.

    This is the only filesystem and parsing step in this module, named
    for what it does so no query hides it. Both extensions are read: a
    coverage lane in the other one would otherwise escape every
    assertion without failing anything.

    Parameters
    ----------
    directory : pathlib.Path or None
        The workflow directory to read. Defaults to the repository's
        own, which is what the contract asserts against.

    Returns
    -------
    dict[str, dict[str, object]]
        File name to parsed document.

    Raises
    ------
    WorkflowReadError
        If a workflow file cannot be read, does not decode as UTF-8, or
        does not parse as YAML. `UnicodeDecodeError` is a `ValueError`
        rather than an `OSError`, so it needs naming separately or a
        workflow with a stray byte escapes the contract this promises.
    """
    root = WORKFLOWS_DIRECTORY if directory is None else directory
    documents: dict[str, dict[str, object]] = {}
    for pattern in ("*.yml", "*.yaml"):
        for path in sorted(root.glob(pattern)):
            try:
                parsed: object = yaml.safe_load(path.read_text(encoding="utf-8"))
            except (OSError, UnicodeError, yaml.YAMLError) as error:
                message = f"cannot read workflow {path}: {error}"
                raise WorkflowReadError(message) from error
            document = mapping_or_empty(parsed)
            if document:
                documents[path.name] = document
    return documents


def _coverage_steps(job: dict[str, object]) -> list[dict[str, object]]:
    """Return the steps in one job that invoke the coverage action.

    Parameters
    ----------
    job : dict[str, object]
        The parsed job.

    Returns
    -------
    list[dict[str, object]]
        The matching steps, in the order the job runs them.
    """
    steps = [mapping_or_empty(step) for step in sequence_or_empty(job.get("steps"))]
    return [step for step in steps if COVERAGE_ACTION in str(step.get("uses", ""))]


def _coverage_job(
    workflow: str,
    document: dict[str, object],
    job_name: str,
    job: dict[str, object],
) -> CoverageJob | None:
    """Return one job's budgets, or None when it runs no coverage step.

    Parameters
    ----------
    workflow : str
        The workflow file's name.
    document : dict[str, object]
        The enclosing document, read for a workflow-level watchdog.
    job_name : str
        The job's identifier.
    job : dict[str, object]
        The parsed job.

    Returns
    -------
    CoverageJob or None
        The job's budgets, or None when it invokes no coverage step.
    """
    steps = _coverage_steps(job)
    if not steps:
        return None
    raw_timeout = job.get("timeout-minutes")
    return CoverageJob(
        workflow=workflow,
        job=job_name,
        steps=len(steps),
        watchdogs=tuple(_watchdog_of(document, job, step) for step in steps),
        job_timeout=None if raw_timeout is None else Fraction(str(raw_timeout)) * 60,
        conditions=tuple((step.get("if"), job.get("if")) for step in steps),
    )


def coverage_jobs_in(documents: WorkflowDocuments) -> tuple[CoverageJob, ...]:
    """Return every job in `documents` invoking the coverage action.

    A pure query over documents the caller has already loaded, so the
    reading is visible at the boundary that does it and the lane
    arithmetic can be exercised against documents built in a test.

    Jobs are the unit rather than steps, because the ceiling is a job's
    and it has to contain every watchdog inside it. Counting steps is
    what makes a job with two invocations visible to the arithmetic.

    Parameters
    ----------
    documents : WorkflowDocuments
        Workflow file name to parsed document, as
        `load_workflow_documents` returns.

    Returns
    -------
    tuple[CoverageJob, ...]
        One entry per coverage-invoking job.
    """
    return tuple(
        found
        for name, document in documents.items()
        for job_name, job in mapping_or_empty(document.get("jobs")).items()
        if (
            found := _coverage_job(name, document, str(job_name), mapping_or_empty(job))
        )
        is not None
    )
