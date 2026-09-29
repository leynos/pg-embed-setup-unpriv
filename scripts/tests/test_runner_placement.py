"""Hold the runner placement to the files: the expression and the ceiling.

A check parametrized over this repository's own correct workflow passes
whether or not it discriminates anything, so the judgement is driven
directly in both directions first: the estate expression must pass, and
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

#: Every job that can land on Ubicloud, with the ceiling it states in
#: minutes. The inventory is exact, so a new Ubicloud lane without a
#: ceiling, or a ceiling removed or changed, fails until it is reviewed.
CEILINGS = [
    (".github/workflows/ci.yml", "build-test", 66),
    (".github/workflows/coverage-main.yml", "coverage-upload", 66),
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
    """Each careless edit is reported, and the estate expression is not."""
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
    assert [(path, job, ceiling) for path, job, _, ceiling in placed] == CEILINGS
    for path, job, runs_on, _ in placed:
        assert not placement_faults(runs_on), f"{path}: {job} is misplaced"
