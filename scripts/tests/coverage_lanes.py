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
    """Raised when the workflow documents cannot be read or parsed.

    Reading is the one filesystem step, so it reports its own failure
    rather than letting a parser's exception surface from what reads
    like a query. An absent or empty directory raises too: returning no
    documents would make every assertion over the lanes vacuous, and a
    contract that passes because it found nothing to check is worse
    than one that fails.
    """


class WorkflowValueError(ValueError):
    """Raised when a workflow declares a duration that is not a number.

    `coverage_jobs_in` is a query over documents the caller supplied,
    but a query still has to say what it does with a value it cannot
    read. `timeout-minutes: ${{ inputs.ceiling }}` and a watchdog set
    from an expression both reach `Fraction` as text, and the bare
    `ValueError` that came back named neither the workflow nor the
    field. This one does, and the query documents it.
    """

    def __init__(self, location: str, field: str, value: object) -> None:
        """Record where the unreadable duration was found.

        Parameters
        ----------
        location : str
            The workflow, and the job within it where applicable.
        field : str
            The key whose value could not be read.
        value : object
            The value as parsed, quoted into the message.
        """
        super().__init__(
            f"{location}: {field} is not a number of seconds: {value!r}"
        )
        self.location = location
        self.field = field
        self.value = value


def _duration(location: str, field: str, raw: object) -> Fraction:
    """Return one declared duration exactly, or refuse it by name.

    Parameters
    ----------
    location : str
        The workflow, and the job within it where applicable.
    field : str
        The key being read, for the message.
    raw : object
        The value as the YAML parser produced it.

    Returns
    -------
    Fraction
        The value, exactly.

    Raises
    ------
    WorkflowValueError
        If the value is not a number. `Fraction` accepts an `int`, a
        `float` and a decimal string; an expression, a duration suffix
        or a null are all refused here rather than escaping as a bare
        `ValueError` from a function documented as a query.
    """
    try:
        return Fraction(str(raw))
    except (ValueError, ZeroDivisionError) as error:
        raise WorkflowValueError(location, field, raw) from error


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
    location: str,
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
    location : str
        The workflow and job, for a refusal message.
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

    Raises
    ------
    WorkflowValueError
        If the innermost level that sets the variable sets it to
        something that is not a number of seconds.
    """
    for owner in (step, job, document):
        raw = mapping_or_empty(owner.get("env")).get(WATCHDOG_VARIABLE)
        if raw is not None:
            return _duration(location, WATCHDOG_VARIABLE, raw)
    return None


def _workflow_paths(root: pathlib.Path) -> list[pathlib.Path]:
    """Return every workflow file under `root`, in a stable order.

    Both extensions are read. A coverage lane written in the other one
    would otherwise escape every assertion downstream without failing
    anything.

    Parameters
    ----------
    root : pathlib.Path
        The workflow directory.

    Returns
    -------
    list[pathlib.Path]
        The files, sorted by name so a failure names the same file on
        every run.
    """
    found = [path for pattern in ("*.yml", "*.yaml") for path in root.glob(pattern)]
    return sorted(found, key=lambda path: path.name)


def _parsed_document(path: pathlib.Path) -> dict[str, object]:
    """Return one workflow file's document, or an empty mapping.

    Parameters
    ----------
    path : pathlib.Path
        The workflow file.

    Returns
    -------
    dict[str, object]
        The parsed mapping, or an empty one when the file holds
        something that is not a mapping at its root.

    Raises
    ------
    WorkflowReadError
        If the file cannot be read, does not decode as UTF-8, or does
        not parse as YAML. `UnicodeDecodeError` is a `ValueError`
        rather than an `OSError`, so it is named separately: a workflow
        carrying a stray byte would otherwise escape the contract the
        caller promises and surface as a decoding error.
    """
    try:
        parsed: object = yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, yaml.YAMLError) as error:
        message = f"cannot read workflow {path}: {error}"
        raise WorkflowReadError(message) from error
    return mapping_or_empty(parsed)


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
        If the directory does not exist, holds no workflow document, or
        a workflow file cannot be read, does not decode as UTF-8, or
        does not parse as YAML. `UnicodeDecodeError` is a `ValueError`
        rather than an `OSError`, so it needs naming separately or a
        workflow with a stray byte escapes the contract this promises.

        The empty cases raise rather than returning nothing, because
        every assertion downstream quantifies over the lanes: with no
        documents the ordering contract passes over an empty set and
        reports success for a repository whose workflows it never read.
    """
    root = WORKFLOWS_DIRECTORY if directory is None else directory
    if not root.is_dir():
        message = f"no workflow directory at {root}"
        raise WorkflowReadError(message)
    documents: dict[str, dict[str, object]] = {}
    for path in _workflow_paths(root):
        document = _parsed_document(path)
        if document:
            documents[path.name] = document
    if not documents:
        message = f"no workflow document parsed under {root}"
        raise WorkflowReadError(message)
    return documents


def _action_coordinate(uses: object) -> str:
    """Return the action a ``uses`` names, without its ref.

    A `uses` value is a coordinate and a ref joined by the first `@`, and
    the ref itself may contain one, so the split is on the first rather
    than the last. A local action path carries no `@` at all and is its
    own coordinate.

    Parameters
    ----------
    uses : object
        The step's `uses` value, whatever the YAML parser produced.

    Returns
    -------
    str
        The coordinate alone.
    """
    return str(uses).partition("@")[0]


def _coverage_steps(job: dict[str, object]) -> list[dict[str, object]]:
    """Return the steps in one job that invoke the coverage action.

    The coordinate is compared for equality rather than containment. A
    substring test also selects a neighbour whose path merely starts
    with this one, such as a `generate-coverage-old` kept beside it
    during a migration, and the contract would then read an unrelated
    action's steps as coverage lanes and assert its watchdogs.

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
    return [
        step
        for step in steps
        if _action_coordinate(step.get("uses", "")) == COVERAGE_ACTION
    ]


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

    Raises
    ------
    WorkflowValueError
        If the job's ``timeout-minutes`` or an in-force watchdog is not
        a number of seconds.
    """
    steps = _coverage_steps(job)
    if not steps:
        return None
    location = f"{workflow}:{job_name}"
    raw_timeout = job.get("timeout-minutes")
    job_timeout = (
        None
        if raw_timeout is None
        else _duration(location, "timeout-minutes", raw_timeout) * 60
    )
    return CoverageJob(
        workflow=workflow,
        job=job_name,
        steps=len(steps),
        watchdogs=tuple(
            _watchdog_of(location, document, job, step) for step in steps
        ),
        job_timeout=job_timeout,
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

    Raises
    ------
    WorkflowValueError
        If a coverage-invoking job declares a ``timeout-minutes`` or a
        watchdog that is not a number of seconds. The query does no
        filesystem work, but it still reads declared values, and a
        value it cannot read is a refusal with a location rather than a
        bare `ValueError` from something documented as a query.
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
