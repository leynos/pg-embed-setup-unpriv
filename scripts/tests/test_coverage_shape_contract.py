"""Contracts holding this repository's workflows to the CV-005 shape.

The rule has two halves and they fail in opposite ways. A pull-request
lane that still calls CodeScene is red whenever the service is, for
reasons no commit here can fix; a trunk publisher that stops uploading
is silently green while the ratchet everyone compares against goes
stale. So the pull-request half is asserted as an absence and the trunk
half as a presence, and each absence is paired with a presence so that
deleting the lane cannot satisfy it.

The rules themselves live in `coverage_shape_rules`, and
`test_coverage_shape_probes` proves each one fires on the hazard it
names; this module applies them to the files in this repository.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from coverage_shape_rules import (
    codescene_contacts,
    measuring_lanes,
    publisher_faults,
    unratcheted_lanes,
)
from workflow_reader import Workflow, load_workflows

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture(scope="module")
def workflows() -> list[Workflow]:
    """Return the repository's parsed workflows once for the module."""
    return load_workflows(REPOSITORY_ROOT)


def test_every_workflow_declares_a_trigger(workflows: list[Workflow]) -> None:
    """Every workflow is read with at least one trigger.

    The presence half that keeps the absences below honest: a reader
    finding no triggers classifies nothing as a pull-request lane and
    passes over a repository that violates every rule here.
    """
    silent = [flow.path for flow in workflows if not flow.triggers]
    assert not silent, f"the reader found no trigger in: {silent}"


def test_a_pull_request_lane_measures_coverage(
    workflows: list[Workflow],
) -> None:
    """Some pull-request coverage step exists and its conditions let it run.

    "No pull-request lane calls CodeScene" is satisfied by a repository
    with no pull-request coverage at all, which is the state this rule
    exists to avoid.
    """
    assert measuring_lanes(workflows), (
        "no pull-request coverage step can run; the ratchet has nothing to compare"
    )


def test_no_pull_request_lane_reaches_codescene(
    workflows: list[Workflow],
) -> None:
    """A pull-request lane, and everything it calls, contacts nothing."""
    offenders = codescene_contacts(workflows)
    assert not offenders, (
        f"a pull-request lane must not reach CodeScene; the service's gate "
        f"configuration is not this repository's to fix: {offenders}"
    )


def test_every_pull_request_coverage_step_ratchets_locally(
    workflows: list[Workflow],
) -> None:
    """The pull-request lane compares against the baseline and keeps its report.

    Without the ratchet the lane measures and compares against nothing;
    with the artefact published it duplicates what the trunk lane
    uploads, under a name a later job may pick up instead.
    """
    wrong = unratcheted_lanes(workflows)
    assert not wrong, f"pull-request coverage steps must ratchet locally: {wrong}"


def test_one_serialized_trunk_publisher_uploads(
    workflows: list[Workflow],
) -> None:
    """Exactly one step uploads, from the trunk, holding the token alone.

    Two publishers race to write the baseline every pull request compares
    against. The publisher also answers `workflow_dispatch`, which runs
    from any branch, so its upload needs the trunk ref as a conjunct no
    disjunction can make optional.
    """
    faults = publisher_faults(workflows)
    assert not faults, f"the trunk publisher is not CV-005 shaped: {faults}"
