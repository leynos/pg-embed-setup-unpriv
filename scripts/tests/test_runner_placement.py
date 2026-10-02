"""Hold the runner placement to the files: the expression and the ceiling.

A check parametrized over this repository's own correct workflow passes
whether or not it discriminates anything, so the judgement is driven
directly in both directions first: the runner-selection expression must pass, and
each way of misplacing a lane must fail. The real files are asserted
last, with an exact inventory of the jobs that can land on Ubicloud.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from runner_placement import (
    ESTATE_EXPRESSION,
    placed_jobs,
    placement_faults,
    selected_runner,
)
from workflow_reader import Workflow, load_workflows

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]

#: Every job that can land on Ubicloud, with the runner class it names and
#: the ceiling it states in minutes. The inventory is exact, so a new
#: Ubicloud lane without a ceiling, or a class or ceiling changed, fails
#: until it is reviewed.
PLACEMENTS = [
    (".github/workflows/ci.yml", "build-test", "ubicloud-standard-4", 66),
    (".github/workflows/ci.yml", "msrv", "ubicloud-standard-4", 30),
    (".github/workflows/ci.yml", "binstall-packaging", "ubicloud-standard-2", 15),
    (".github/workflows/coverage-main.yml", "coverage-upload", "ubicloud-standard-2", 66),
]


@pytest.mark.parametrize(
    ("origin", "wanted"),
    [
        ("push", "ubicloud-standard-2"),
        ("same-repository", "ubicloud-standard-2"),
        ("fork", "ubuntu-latest"),
    ],
)
def test_the_estate_expression_places_each_run(origin: str, wanted: str) -> None:
    """A push, a dispatch and a same-repository pull request get Ubicloud.

    Only a fork's pull request falls back to the hosted pool.
    """
    assert selected_runner(ESTATE_EXPRESSION, origin) == wanted


@pytest.mark.parametrize(
    ("runs_on", "expected"),
    [
        (ESTATE_EXPRESSION, 0),
        ("ubuntu-latest", 3),
        ("ubicloud-standard-2", 3),
        (
            "${{ github.event.pull_request.head.repo.fork && "
            "'ubicloud-standard-2' || 'ubuntu-latest' }}",
            3,
        ),
        (
            "${{ github.event.pull_request.head.repo.fork && 'ubuntu-latest' "
            "|| 'ubicloud-standard-4' }}",
            2,
        ),
        (
            "${{ github.event_name == 'pull_request' && 'ubuntu-latest' "
            "|| 'ubicloud-standard-2' }}",
            3,
        ),
        (
            "${{ github.event.pull_request.head.repo.fork && "
            "'ubicloud-standard-2' || 'ubicloud-standard-2' }}",
            1,
        ),
        (None, 3),
    ],
    ids=[
        "estate",
        "always-hosted",
        "always-ubicloud",
        "inverted-arms",
        "another-label",
        "another-condition",
        "fork-kept-on-ubicloud",
        "not-a-string",
    ],
)
def test_a_misplaced_lane_is_reported(runs_on: object, expected: int) -> None:
    """Each careless edit is reported, and the runner-selection expression is not."""
    assert len(placement_faults(runs_on)) == expected


@pytest.mark.parametrize(
    ("key", "expected"),
    [
        ("    timeout-minutes: 30\n", 30),
        ("", None),
        ("    timeout-minutes: thirty\n", "thirty"),
    ],
    ids=["stated", "missing", "a-string"],
)
def test_a_ceiling_is_read_as_the_file_states_it(
    key: str, expected: object
) -> None:
    """The inventory reads the ceiling verbatim, so a wrong one cannot pass."""
    text = f"on: push\njobs:\n  lane:\n    runs-on: {ESTATE_EXPRESSION}\n{key}"
    flow = Workflow.parse("x.yml", text)
    assert placed_jobs([flow]) == [("x.yml", "lane", ESTATE_EXPRESSION, expected)]


def test_a_non_ubicloud_job_is_not_inventoried() -> None:
    """A hosted lane is outside the inventory: it needs no ceiling."""
    flow = Workflow.parse("x.yml", "on: push\njobs:\n  lane:\n    runs-on: ubuntu-latest\n")
    assert placed_jobs([flow]) == []


def test_every_ubicloud_lane_is_placed_by_the_estate_expression_and_states_a_ceiling() -> (
    None
):
    """Exactly the inventoried jobs can land on Ubicloud, each ceiling stated."""
    placed = placed_jobs(load_workflows(REPOSITORY_ROOT))
    assert [(path, job, ceiling) for path, job, _, ceiling in placed] == [
        (path, job, ceiling) for path, job, _, ceiling in PLACEMENTS
    ]
    for (path, job, runs_on, _), (_, _, label, _) in zip(placed, PLACEMENTS, strict=True):
        assert not placement_faults(runs_on, label), f"{path}: {job} is misplaced"


def test_a_runner_class_other_than_the_inventoried_one_is_reported() -> None:
    """A lane on the wrong class of Ubicloud runner is a fault, in both directions."""
    four = ESTATE_EXPRESSION.replace("standard-2", "standard-4")
    assert placement_faults(four, "ubicloud-standard-4") == []
    assert len(placement_faults(four, "ubicloud-standard-2")) == 2


def _matrix_job(reference: str, row: str) -> Workflow:
    """Return a one-job workflow reading a matrix runner, with one row set.

    Parameters
    ----------
    reference : str
        The job's `runs-on`, reading the matrix.
    row : str
        The runner value of the Linux row, as YAML text.

    Returns
    -------
    Workflow
        The parsed workflow, with a hosted macOS row beside the Linux one.
    """
    text = (
        f"on: push\njobs:\n  lane:\n    runs-on: {reference}\n"
        "    strategy:\n      matrix:\n        include:\n"
        f"          - runner: {row}\n            target: linux\n"
        "          - runner: macos-latest\n            target: macos\n"
    )
    return Workflow.parse("x.yml", text)


@pytest.mark.parametrize(
    ("reference", "row", "expected"),
    [
        ("${{ matrix.runner }}", f'"{ESTATE_EXPRESSION}"', 0),
        ("${{ matrix['runner'] }}", f'"{ESTATE_EXPRESSION}"', 0),
        ("${{ matrix.runner }}", "ubicloud-standard-2", 3),
        (
            "${{ matrix.runner }}",
            '"${{ github.event.pull_request.head.repo.fork && '
            "'ubicloud-standard-2' || 'ubuntu-latest' }}\"",
            3,
        ),
        (
            "${{ matrix.runner }}",
            '"${{ github.event.pull_request.head.repo.fork && '
            "'ubuntu-latest' || 'ubicloud-standard-4' }}\"",
            2,
        ),
    ],
    ids=["dotted", "indexed", "literal-label", "inverted-arms", "another-class"],
)
def test_a_matrix_row_is_judged_on_its_own_expression(
    reference: str, row: str, expected: int
) -> None:
    """A placement taken from a matrix row is inventoried and judged on that row.

    The hosted macOS row is left alone, so a job placed through one Linux
    row yields exactly one entry, and each wrong placement of that row is
    reported.
    """
    placed = placed_jobs([_matrix_job(reference, row)])
    assert len(placed) == 1, f"expected the one Ubicloud row, read {placed}"
    assert len(placement_faults(placed[0][2])) == expected, placed


def test_a_matrix_of_hosted_rows_is_not_inventoried() -> None:
    """A matrix that never names Ubicloud needs no ceiling."""
    flow = _matrix_job("${{ matrix.runner }}", "ubuntu-latest")
    assert placed_jobs([flow]) == []


def test_a_matrix_naming_ubicloud_elsewhere_is_inventoried_and_rejected() -> None:
    """Refuse Ubicloud named under a key the `runs-on` does not read."""
    text = (
        "on: push\njobs:\n  lane:\n    runs-on: ${{ matrix.os }}\n"
        "    strategy:\n      matrix:\n        include:\n"
        "          - os: ubuntu-latest\n            runner: ubicloud-standard-2\n"
    )
    placed = placed_jobs([Workflow.parse("x.yml", text)])
    assert len(placed) == 1, f"not inventoried: {placed}"
    assert placement_faults(placed[0][2]), f"not rejected: {placed}"


def test_a_fork_may_fall_back_to_the_pinned_ubuntu_release() -> None:
    """A lane that pins an Ubuntu release keeps it on the fork arm.

    Both hosted labels are accepted for the fork, and any other is not.
    """
    pinned = ESTATE_EXPRESSION.replace("'ubuntu-latest'", "'ubuntu-24.04'")
    other = ESTATE_EXPRESSION.replace("'ubuntu-latest'", "'ubuntu-22.04'")
    assert placement_faults(pinned) == []
    assert len(placement_faults(other)) == 1
