"""Contract for the timers that can end a test run.

Four independent budgets can end a coverage lane, each set somewhere
different, and they only work if each sits above the one inside it. All
four are set here once this contract lands: a per-test ``slow-timeout``
and a whole-run ``global-timeout`` in ``.config/nextest.toml``, the
shared coverage action's wall-clock watchdog on the ``cargo``
invocation, and the job's own ``timeout-minutes``.

The outermost tier was the one missing. Neither coverage job declared
``timeout-minutes``, so both inherited GitHub's six-hour default while
the pull-request lane already ran for twenty-two minutes.

The contract also holds the inner ordering, which this repository does
satisfy: a 600 s whole-run budget above a 360 s largest per-test
allowance, and a 1,800 s watchdog above that budget once nextest's
termination procedure and a cold build are counted.

The readings these assertions rest on live in ``timeout_budgets.py`` and
``coverage_lanes.py``, and are exercised in
``test_timeout_reading_contract.py``.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``, and the canonical wording in
`leynos/shared-actions`' `generate-coverage` README.
"""

import typing as typ

import pytest
from coverage_lanes import CoverageJob, coverage_jobs_of
from nextest_budgets import (
    global_timeout,
    largest_test_allowance,
    termination_allowance,
)
from timeout_budgets import (
    CEILING_MARGIN_SECONDS,
    COLD_BUILD_ALLOWANCE_SECONDS,
    COVERAGE_ACTION,
    NEXTEST_CONFIG,
    OUTSIDE_WATCHDOG_ALLOWANCE_SECONDS,
    REQUIRED_GLOBAL_TIMEOUT_SECONDS,
    REQUIRED_JOB_CEILING_SECONDS,
    WATCHDOG_VARIABLE,
    required_ceiling,
)


#: The condition each coverage lane legitimately carries, keyed by
#: workflow and job, as the step's ``if`` and its job's.
#:
#: A skipped step runs no `cargo`, so its watchdog never arms and every
#: assertion below says nothing about it. `if: false` on either would
#: leave a lane that looks bounded and is not. The values are pinned
#: rather than merely tolerated, because a lane gaining, losing or
#: changing a condition changes when it runs at all.
#:
#: `ci.yml`'s coverage step is the unprivileged leg of a matrix that
#: also runs as root, and only the unprivileged leg measures coverage.
#: `coverage-main.yml` runs on the trunk and carries no condition.
REQUIRED_CONDITIONS: typ.Final[dict[tuple[str, str], tuple[object, object]]] = {
    ("ci.yml", "build-test"): ("${{ matrix.privilege == 'unprivileged' }}", None),
    ("coverage-main.yml", "coverage-upload"): (None, None),
}


@pytest.fixture(scope="module")
def nextest_config() -> str:
    """Return the nextest configuration file's text.

    Returns
    -------
    str
        The file's contents.
    """
    return NEXTEST_CONFIG.read_text(encoding="utf-8")


@pytest.fixture(scope="module")
def coverage_jobs() -> tuple[CoverageJob, ...]:
    """Return every job invoking the coverage action, with its budgets.

    Returns
    -------
    tuple[CoverageJob, ...]
        One entry per coverage-invoking job.
    """
    return coverage_jobs_of()


def test_the_coverage_action_is_invoked_somewhere(
    coverage_jobs: tuple[CoverageJob, ...],
) -> None:
    """The contract needs a job to assert against.

    A repin or a rename that stopped the coordinate matching would
    otherwise turn every assertion below into a vacuous pass over an
    empty list, and the loss would look exactly like success.
    """
    assert coverage_jobs, (
        f"no workflow job uses {COVERAGE_ACTION}; either coverage moved or "
        f"this contract stopped recognizing it"
    )


def test_every_coverage_step_runs_under_an_explicit_watchdog(
    coverage_jobs: tuple[CoverageJob, ...],
) -> None:
    """The default is invisible, so every step must write it down.

    The action kills `cargo` after 1,800 s unless told otherwise, and the
    value here equals that default, which makes writing it down more
    important rather than less: an accidental deletion would change
    nothing observable until the run it killed.
    """
    missing = [
        f"{job}: step {index + 1} of {job.steps}"
        for job in coverage_jobs
        for index, watchdog in enumerate(job.watchdogs)
        if watchdog is None
    ]
    assert not missing, (
        f"these coverage steps do not set {WATCHDOG_VARIABLE} and so inherit "
        f"the shared action's undocumented default: {missing}"
    )


def test_the_job_ceiling_contains_every_watchdog_and_the_work_around_them(
    coverage_jobs: tuple[CoverageJob, ...],
) -> None:
    """Tier four must not pre-empt tier three, for any of the invocations.

    Each coverage step gets its own watchdog, so a job running the action
    twice can legitimately spend both budgets, and its ceiling has to
    contain the sum rather than one of them. The clocks do not start
    together either: the job timer starts before the checkout and runs
    through the cache saves afterwards, which on Windows are the largest
    thing in the job outside the coverage steps themselves.

    A ceiling merely above one watchdog cancels the job partway through
    the second invocation, and a cancellation discards the log that would
    have explained it.
    """
    for job in coverage_jobs:
        budgets = [watchdog for watchdog in job.watchdogs if watchdog is not None]
        assert len(budgets) == job.steps, str(job)
        allowance = OUTSIDE_WATCHDOG_ALLOWANCE_SECONDS
        required = required_ceiling(budgets, allowance)
        assert job.job_timeout is not None, (
            f"{job} runs {job.steps} watchdog-bounded cargo invocation(s) in a "
            f"job with no timeout-minutes; the outermost tier is missing and "
            f"GitHub's six-hour default applies"
        )
        assert job.job_timeout == pytest.approx(REQUIRED_JOB_CEILING_SECONDS), (
            f"{job} has a ceiling of {job.job_timeout:.0f}s, not the "
            f"{REQUIRED_JOB_CEILING_SECONDS:.0f}s the developers' guide "
            f"states; the derivation below accepts a range, so only this "
            f"pin keeps the guide and the workflows one statement"
        )
        assert job.job_timeout >= required, (
            f"{job} has a ceiling of {job.job_timeout:.0f}s, below the "
            f"{required:.0f}s needed to contain {job.steps} watchdog(s) "
            f"totalling {sum(budgets):.0f}s, {allowance:.0f}s of measured "
            f"work outside them, and a {CEILING_MARGIN_SECONDS:.0f}s margin "
            f"above that sum; an overrun would be cancelled rather than "
            f"reported"
        )


def test_the_whole_run_budget_is_the_one_the_guide_states(
    nextest_config: str,
) -> None:
    """Tier two is present here, and is required to stay present.

    Skipping when no ``global-timeout`` is found would let this tier be
    deleted and leave the four-tier contract passing with three, which
    is the state the branch exists to correct. The value is pinned as
    well, because the ordering below holds for a wide range of budgets
    and would not notice this one drifting away from the guide.
    """
    whole_run = global_timeout(nextest_config)
    assert whole_run is not None, (
        "`.config/nextest.toml` must set a global-timeout; without it "
        "nothing bounds the whole run and the watchdog reports the cargo "
        "invocation rather than the suite"
    )
    assert whole_run == pytest.approx(REQUIRED_GLOBAL_TIMEOUT_SECONDS), (
        f"the global-timeout is {whole_run:.0f}s, not the "
        f"{REQUIRED_GLOBAL_TIMEOUT_SECONDS:.0f}s the developers' guide "
        f"states; change the guide with it or change it back"
    )


def test_the_whole_run_budget_sits_inside_each_watchdog(
    coverage_jobs: tuple[CoverageJob, ...], nextest_config: str
) -> None:
    """Tier three must not pre-empt tier two.

    The watchdog names `cargo`, not the suite, so a watchdog below the
    whole-run budget would kill the invocation before nextest could
    report which test had overrun, and the report is the only thing that
    makes the overrun actionable.
    """
    whole_run = global_timeout(nextest_config)
    assert whole_run is not None, "no global-timeout is set"
    largest = largest_test_allowance(nextest_config)
    assert whole_run > largest, (
        f"the {whole_run:.0f}s global-timeout is not above the {largest:.0f}s "
        f"largest per-test allowance; the run would end before that test "
        f"could use its budget"
    )
    required = (
        whole_run + termination_allowance(nextest_config) + COLD_BUILD_ALLOWANCE_SECONDS
    )
    for job in coverage_jobs:
        for index, watchdog in enumerate(job.watchdogs):
            assert watchdog is not None, str(job)
            assert watchdog >= required, (
                f"{job} step {index + 1} sets a {watchdog:.0f}s watchdog, "
                f"below the {required:.0f}s needed to cover the "
                f"{whole_run:.0f}s whole-run budget, nextest's termination "
                f"procedure, and a cold build"
            )


def test_each_coverage_lane_carries_the_condition_it_is_meant_to(
    coverage_jobs: tuple[CoverageJob, ...],
) -> None:
    """A skipped step runs no `cargo`, so its watchdog never arms.

    Every assertion above reads a lane's declared budgets and says
    nothing about whether the step runs. `if: false` on the step or on
    its job would leave a lane that looks bounded and is not, and this
    contract would certify it. So would a plausible condition that
    quietly excluded the event the lane exists for.

    The conditions are pinned rather than forbidden, because the one
    here is legitimate: `ci.yml` runs the coverage step on the
    unprivileged leg of a matrix that also runs as root. The
    coordinates are compared both ways first, so a new lane with no
    entry here fails rather than passing unexamined, and a lane that
    disappeared fails rather than being skipped.

    Proved by mutation: `if: false` on the coverage step, the same on
    its job, the matrix condition changed to the root leg, and a
    coordinate dropped from ``REQUIRED_CONDITIONS`` each fail this test.
    """
    found = {(job.workflow, job.job): job.conditions for job in coverage_jobs}
    assert set(found) == set(REQUIRED_CONDITIONS), (
        f"the coverage lanes are not the ones this contract pins: "
        f"unlisted {sorted(set(found) - set(REQUIRED_CONDITIONS))}, missing "
        f"{sorted(set(REQUIRED_CONDITIONS) - set(found))}; a lane with no "
        f"entry here is a lane whose condition nobody has judged"
    )
    wrong = {
        coordinate: (expected, found[coordinate])
        for coordinate, expected in REQUIRED_CONDITIONS.items()
        if set(found[coordinate]) != {expected}
    }
    assert not wrong, (
        f"these coverage lanes do not carry the conditions the developers' "
        f"guide records, as expected versus found: {wrong}; a lane that is "
        f"skipped runs no cargo, so its watchdog never arms"
    )
