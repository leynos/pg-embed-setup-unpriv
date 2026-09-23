"""Drive the rule that a pull-request coverage step must be able to run.

A coverage step found by its action alone passes whatever guards it, so
`if: false` or a matrix value no leg carries leaves the contract green
while nothing measures. Each case builds the pull-request workflow the
probes suite uses, applies one guard, and asserts whether the step still
counts; the narrow cases keep the repository's own matrix guard counting.
"""

from __future__ import annotations

import typing as typ

import pytest
from coverage_shape_rules import measuring_lanes
from test_coverage_shape_probes import CI, repository

#: The step's `with:` line, where a step-level guard is inserted.
STEP: typ.Final = "        with:\n"

#: The job's `runs-on:` line, where job-level keys are inserted.
JOB: typ.Final = "    runs-on: ubuntu-latest\n"


def guarded(step_if: str | None = None, job_lines: str = "") -> str:
    """Return the pull-request workflow with a step guard and job keys added."""
    ci = CI.replace(JOB, JOB + job_lines)
    return (
        ci if step_if is None else ci.replace(STEP, f"        if: {step_if}\n" + STEP)
    )


def matrix(lines: str) -> str:
    """Return job lines declaring a matrix from indented YAML lines."""
    return "    strategy:\n      matrix:\n" + lines


def test_the_base_pull_request_lane_measures() -> None:
    """The narrow half: the unconditioned base step is a measuring lane."""
    assert len(measuring_lanes(repository())) == 1, (
        "the base workflow's unconditioned coverage step must count as measuring"
    )


#: Guards that leave the pull-request coverage step unable to run.
NEVER_RUNS: typ.Final = {
    "the step disabled": guarded("false"),
    "the job disabled": guarded(job_lines="    if: ${{ false }}\n"),
    "a matrix value no leg carries": guarded(
        "${{ matrix.privilege == 'unprivileged' }}",
        matrix("        privilege: [root]\n"),
    ),
    "a push-only event guard": guarded("github.event_name == 'push'"),
    # Each value exists, but no single leg carries both.
    "two include rows that never meet": guarded(
        "matrix.os == 'linux' && matrix.privilege == 'unprivileged'",
        matrix(
            "        include:\n"
            "          - os: linux\n"
            "          - privilege: unprivileged\n"
        ),
    ),
}


@pytest.mark.parametrize("ci", NEVER_RUNS.values(), ids=NEVER_RUNS.keys())
def test_a_coverage_step_that_cannot_run_does_not_measure(ci: str) -> None:
    """A step found by its action but kept from running measures nothing."""
    found = measuring_lanes(repository(ci=ci))
    assert found == [], f"a coverage step that cannot run was counted: {found}"


#: Guards some leg of the matrix satisfies.
RUNS: typ.Final = {
    "the repository's own matrix guard": guarded(
        "${{ matrix.privilege == 'unprivileged' }}",
        matrix("        privilege: [unprivileged, root]\n"),
    ),
    "one include row carrying both values": guarded(
        "matrix.os == 'linux' && matrix.privilege == 'unprivileged'",
        matrix(
            "        include:\n"
            "          - os: linux\n"
            "            privilege: unprivileged\n"
        ),
    ),
    "an include row extending a crossed leg": guarded(
        "matrix.os == 'linux' && matrix.privilege == 'unprivileged'",
        matrix(
            "        os: [linux, mac]\n"
            "        include:\n"
            "          - os: linux\n"
            "            privilege: unprivileged\n"
        ),
    ),
}


@pytest.mark.parametrize("ci", RUNS.values(), ids=RUNS.keys())
def test_a_guard_some_leg_satisfies_still_measures(ci: str) -> None:
    """The narrow half: a guard one real leg satisfies keeps the step counting."""
    found = measuring_lanes(repository(ci=ci))
    assert len(found) == 1, f"a coverage step some leg runs was not counted: {found}"
