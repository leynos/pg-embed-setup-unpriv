"""Prove the `codescene` environment sits on the uploading job and nowhere else.

The first test applies the rule to this repository's workflows. The rest
start from the constructed repository the coverage-shape probes use, with
the environment declared on its publisher, and make the one edit a later
change could, asserting the clause meant to catch it fires.
"""

from __future__ import annotations

import typing as typ
from pathlib import Path

from codescene_environment import (
    MISSING,
    NO_UPLOADER,
    REACHABLE,
    STRAY,
    environment_faults,
)
from test_coverage_shape_probes import CI, PUBLISHER, repository
from workflow_reader import load_workflows

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]

#: Where the environment goes in the constructed publisher's one job.
PUBLISHER_JOB: typ.Final = "  upload:\n    runs-on: ubuntu-latest\n"

#: The constructed publisher with the environment declared.
PLACED: typ.Final = PUBLISHER.replace(
    PUBLISHER_JOB, PUBLISHER_JOB + "    environment: codescene\n"
)


def faults_with(publisher: str = PLACED, ci: str = CI) -> list[str]:
    """Return the environment faults of the constructed repository."""
    return environment_faults(repository(ci=ci, publisher=publisher))


def test_this_repository_places_the_environment() -> None:
    """The real publisher declares the environment and nothing else does."""
    found = environment_faults(load_workflows(REPOSITORY_ROOT))
    assert found == [], found


def test_the_base_places_the_environment() -> None:
    """The constructed base is clean, so each probe's failure is its own."""
    assert PUBLISHER_JOB in PUBLISHER, "the probe's anchor is not in the base"
    found = faults_with()
    assert found == [], found


def test_the_publisher_cannot_drop_the_environment() -> None:
    """Without it the job runs outside the environment holding the token."""
    found = faults_with(publisher=PUBLISHER)
    assert any(MISSING in fault for fault in found), found


def test_the_publisher_cannot_name_another_environment() -> None:
    """Another environment has neither the token nor the main-only policy."""
    found = faults_with(publisher=PLACED.replace("codescene\n", "production\n", 1))
    assert any(MISSING in fault for fault in found), found


def test_the_mapping_form_is_accepted() -> None:
    """`{name: codescene}` is the same declaration as the bare string."""
    found = faults_with(
        publisher=PLACED.replace(
            "environment: codescene\n", "environment: {name: codescene}\n"
        )
    )
    assert found == [], found


def test_no_other_job_may_declare_it() -> None:
    """A second job holding the environment widens what can read the token."""
    other = "  other:\n    runs-on: ubuntu-latest\n    environment: codescene\n"
    found = faults_with(publisher=PLACED + other + "    steps: [{run: 'true'}]\n")
    assert any(STRAY in fault for fault in found), found


def test_no_pull_request_job_may_declare_it() -> None:
    """A pull request's own code must never be able to request the token."""
    ci = CI.replace(
        "    runs-on: ubuntu-latest\n",
        "    runs-on: ubuntu-latest\n    environment: {name: codescene}\n",
        1,
    )
    assert ci != CI, "the pull-request probe's anchor is not in the base"
    found = faults_with(ci=ci)
    assert any(REACHABLE in fault for fault in found), found


def test_an_empty_reading_is_refused() -> None:
    """With no uploader left the rule says so rather than passing."""
    uploader = "leynos/shared-actions/.github/actions/upload-codescene-coverage@abc"
    found = faults_with(publisher=PLACED.replace(uploader, "actions/checkout@abc"))
    assert found == [NO_UPLOADER], found
