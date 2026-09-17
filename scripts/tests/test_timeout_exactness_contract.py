"""The tier comparisons stay exact all the way from the files they read.

`test_timeout_reading_contract.py` proves that a nextest duration is read
exactly. That is only half of it. Every tier comparison is a sum, and each
sum mixes a duration read from `.config/nextest.toml` with a budget read
from a workflow and with a constant declared in `timeout_budgets`. A sum
is exact only if every term is: one `float` among them converts the whole
of it back, and the conversion is silent.

So this module drives the three compositions the ordering contract
actually evaluates, each with two inputs one second apart and large
enough that a float cannot hold both. The repository's own budgets are
nowhere near that magnitude and never will be, which is precisely why the
loss cannot be exposed by the real files: a contract resting on them
would pass with every term a float.

Each case asserts the float collapse alongside the exact comparison. The
collapse is the thing being avoided rather than an incidental detail, and
a case that stopped exercising it would keep passing while proving
nothing.
"""

import typing as typ
from fractions import Fraction

from coverage_lanes import coverage_jobs_in
from nextest_budgets import global_timeout, termination_allowance
from timeout_budgets import (
    COLD_BUILD_ALLOWANCE_SECONDS,
    COVERAGE_ACTION,
    OUTSIDE_WATCHDOG_ALLOWANCE_SECONDS,
    WATCHDOG_VARIABLE,
    required_ceiling,
    seconds,
)

#: A magnitude whose neighbouring floats are 256 seconds apart, so a
#: one-second difference is lost outright rather than only sometimes.
#: Two to the fifty-third is the first magnitude that loses anything, but
#: there the spacing is two and whether a given pair collapses depends on
#: which side of a tie it falls; at two to the sixtieth every difference
#: below 128 seconds vanishes, which is what these cases need.
HUGE_SECONDS: typ.Final[int] = 2**60


def _workflow(watchdog: int, timeout_minutes: int) -> dict[str, dict[str, object]]:
    """Return one document declaring a single coverage job.

    Parameters
    ----------
    watchdog : int
        The watchdog budget in seconds, set at job level as both real
        workflows set it.
    timeout_minutes : int
        The job's ``timeout-minutes``.

    Returns
    -------
    dict[str, dict[str, object]]
        A document map as ``load_workflow_documents`` returns.
    """
    return {
        "ci.yml": {
            "jobs": {
                "coverage": {
                    "timeout-minutes": timeout_minutes,
                    "env": {WATCHDOG_VARIABLE: watchdog},
                    "steps": [{"uses": f"{COVERAGE_ACTION}@" + "0" * 40}],
                }
            }
        }
    }


def _watchdog_read(watchdog: int) -> Fraction:
    """Return the watchdog budget the lane reader takes from a document.

    Parameters
    ----------
    watchdog : int
        The value to declare.

    Returns
    -------
    Fraction
        What the reader made of it.
    """
    (job,) = coverage_jobs_in(_workflow(watchdog, timeout_minutes=66))
    (budget,) = job.watchdogs
    assert budget is not None, "the job declares a watchdog at job level"
    return budget


def _job_timeout_read(timeout_minutes: int) -> Fraction:
    """Return the job ceiling the lane reader takes from a document.

    Parameters
    ----------
    timeout_minutes : int
        The ``timeout-minutes`` to declare.

    Returns
    -------
    Fraction
        The ceiling in seconds.
    """
    (job,) = coverage_jobs_in(_workflow(1800, timeout_minutes=timeout_minutes))
    assert job.job_timeout is not None, "the job declares timeout-minutes"
    return job.job_timeout


def test_the_job_ceiling_reader_keeps_a_minute_a_float_would_lose() -> None:
    """Two ceilings one minute apart must not read as one value.

    The ceiling is the other value read from a workflow, and it is
    converted as well as read: ``timeout-minutes`` is multiplied by
    sixty. Both the reading and the multiplication were done in floating
    point, so the comparison that holds a job above the sum of its
    watchdogs would have accepted a ceiling a minute short of it.
    """
    larger = _job_timeout_read(HUGE_SECONDS + 1)
    smaller = _job_timeout_read(HUGE_SECONDS)

    assert larger > smaller, "ceilings one minute apart must order strictly"
    assert float(larger) == float(smaller), (
        "the float collapse this guards against must still be real; if these "
        "differ, the case no longer exercises what it was written for"
    )


def test_the_lane_reader_keeps_a_second_a_float_would_lose() -> None:
    """Two watchdog budgets one second apart must not read as one value.

    The workflow side was read with `float(str(raw))`, so the exactness
    won on the nextest side was discarded the moment a watchdog entered
    the comparison. The job ceiling and the watchdog floor are both
    comparisons against a workflow value, so both inherited it.
    """
    larger = _watchdog_read(HUGE_SECONDS + 1)
    smaller = _watchdog_read(HUGE_SECONDS)

    assert larger > smaller, "watchdogs one second apart must order strictly"
    assert float(larger) == float(smaller), (
        "the float collapse this guards against must still be real; if these "
        "differ, the case no longer exercises what it was written for"
    )


def test_the_job_ceiling_arithmetic_stays_exact() -> None:
    """The required ceiling is a sum, and a sum is as exact as its terms.

    `required_ceiling` adds the watchdog budgets, the measured work
    outside them and the margin. The margin was a `float`, so the total
    was a `float` whatever the budgets were, and the ceiling comparison
    that reads it would accept a job one second short of containing its
    watchdogs. The allowance is the constant the ordering contract
    passes rather than a stand-in, so a `float` in either term is caught
    here rather than in whichever of them happens to be exercised.
    """
    allowance = OUTSIDE_WATCHDOG_ALLOWANCE_SECONDS
    larger = required_ceiling([Fraction(HUGE_SECONDS + 1)], allowance)
    smaller = required_ceiling([Fraction(HUGE_SECONDS)], allowance)

    assert larger > smaller, "ceilings one second apart must order strictly"
    assert float(larger) == float(smaller), (
        "the float collapse this guards against must still be real; if these "
        "differ, the case no longer exercises what it was written for"
    )


def _watchdog_floor(whole_run: int, *, grace_period: bool = True) -> Fraction:
    """Return the watchdog floor the ordering contract derives.

    This is the expression that contract evaluates, kept in one place so
    a later edit there and here cannot silently disagree.

    Parameters
    ----------
    whole_run : int
        The whole-run budget in seconds.
    grace_period : bool
        Whether the profile declares a ``grace-period``. Without one
        nextest's default applies, and that default is a term of the
        floor like any other, so both spellings are driven.

    Returns
    -------
    Fraction
        The smallest watchdog that covers the whole run, nextest's
        termination procedure and a cold build.
    """
    grace = ', grace-period = "5s"' if grace_period else ""
    config_text = (
        "[profile.default]\n"
        f'global-timeout = "{whole_run}s"\n'
        f'slow-timeout = {{ period = "60s", terminate-after = 1{grace} }}\n'
    )
    budget = global_timeout(config_text)
    assert budget is not None, "the profile declares a global-timeout"
    return budget + termination_allowance(config_text) + COLD_BUILD_ALLOWANCE_SECONDS


def test_the_watchdog_floor_stays_exact() -> None:
    """The watchdog floor adds two constants to an exact budget.

    Both the safety margin and the cold-build allowance were `float`, so
    the floor a watchdog is compared against was a `float` even though
    the whole-run budget reaching it was exact. The configured grace
    period and nextest's default are both driven, because the default is
    a fourth constant and a reader exercising only the configured
    spelling would never reach it.
    """
    for grace_period in (True, False):
        larger = _watchdog_floor(HUGE_SECONDS + 1, grace_period=grace_period)
        smaller = _watchdog_floor(HUGE_SECONDS, grace_period=grace_period)

        assert larger > smaller, (
            f"floors one second apart must order strictly "
            f"(grace-period declared: {grace_period})"
        )
        assert float(larger) == float(smaller), (
            "the float collapse this guards against must still be real; if "
            "these differ, the case no longer exercises what it was written "
            f"for (grace-period declared: {grace_period})"
        )


def test_the_configured_tiers_are_exact_values() -> None:
    """The real files read exactly too, not merely the constructed ones.

    The cases above use magnitudes this repository will never configure,
    which is what makes them able to see the loss. This one is the other
    half: the values actually in force arrive as exact numbers, so a
    reader reintroducing a float at any point in the real path is caught
    without waiting for a budget no one will ever set.
    """
    assert isinstance(seconds("600s"), Fraction), (
        "a nextest duration must arrive exact"
    )
    assert isinstance(_watchdog_read(1800), Fraction), (
        "a workflow watchdog must arrive exact"
    )
    assert isinstance(_job_timeout_read(66), Fraction), (
        "a job ceiling must arrive exact through its conversion to seconds"
    )
    assert isinstance(required_ceiling([Fraction(1800)], Fraction(1200)), Fraction), (
        "the ceiling must stay exact through its sum"
    )
