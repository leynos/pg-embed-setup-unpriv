"""The values the timeout contract compares, and the terms it derives.

Separated from the nextest reading in ``nextest_budgets`` and the
workflow reading in ``coverage_lanes`` so each stays legible on its own
and none outgrows the 400-line limit ``AGENTS.md`` sets.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

import collections.abc as cabc
import typing as typ
from pathlib import Path

REPO_ROOT: typ.Final[Path] = Path(__file__).resolve().parents[2]
WORKFLOWS_DIRECTORY: typ.Final[Path] = REPO_ROOT / ".github" / "workflows"
NEXTEST_CONFIG: typ.Final[Path] = REPO_ROOT / ".config" / "nextest.toml"

#: The environment variable the shared coverage action reads for its
#: wall-clock cap on one `cargo` invocation.
WATCHDOG_VARIABLE: typ.Final[str] = "RUN_RUST_CARGO_WAIT_TIMEOUT"

#: The action whose steps run under that watchdog.
COVERAGE_ACTION: typ.Final[str] = (
    "leynos/shared-actions/.github/actions/generate-coverage"
)

#: Everything in a coverage job that is not a `cargo` invocation the
#: watchdog bounds: the checkout and toolchain setup, the cache save and
#: restore, and here also the suite's own `cargo nextest` step and the
#: Loom models, which run outside the coverage step. The job timer
#: covers all of it; the watchdog does not.
#:
#: Measured from the worst of many runs rather than one, and across runs
#: of every conclusion rather than successful ones only. Across the last
#: 115 `ci.yml` coverage jobs, 44 successful, 55 failed and 16
#: cancelled, the widest gap between the coverage step and its job was
#: 969 s on run 30024924292. Across all 19 `coverage-main.yml` runs it
#: was 42 s on run 29354687551. No cancelled run came near the ceiling,
#: the worst reaching 727 s of a 3,600 s budget, so none was ended by
#: any of these four timers. Twenty minutes covers the worst gap seen
#: with eleven minutes to spare, and none of those runs was genuinely
#: cold.
OUTSIDE_WATCHDOG_ALLOWANCE_SECONDS: typ.Final[float] = 20 * 60.0

#: What nextest allows a test between `SIGTERM` and `SIGKILL` when the
#: configuration names no `grace-period`. This one names 5 s, so the
#: default is a fallback rather than the value in force.
NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS: typ.Final[float] = 10.0

#: Added to that grace period to cover the teardown and report writing
#: that follow it. Kept a separate term rather than folded into a single
#: floor, so raising the grace period raises the requirement instead of
#: being absorbed silently.
TERMINATION_SAFETY_MARGIN_SECONDS: typ.Final[float] = 60.0

#: Build time inside a `cargo` invocation before nextest starts its own
#: clock.
COLD_BUILD_ALLOWANCE_SECONDS: typ.Final[float] = 10 * 60.0

#: The whole-run budget `.config/nextest.toml` must declare, as
#: `docs/developers-guide.md` records it. Pinned as well as ordered: the
#: ordering below holds for a wide range of values and would not notice
#: the budget drifting away from the guide.
REQUIRED_GLOBAL_TIMEOUT_SECONDS: typ.Final[float] = 10 * 60.0

#: How far a ceiling must sit above the sum it contains, rather than
#: merely reaching it. A ceiling equal to that sum cancels the job at
#: the moment the watchdog would have reported the overrun, and the
#: report is the only thing that makes an overrun actionable.
CEILING_MARGIN_SECONDS: typ.Final[float] = 15 * 60.0

#: The ceiling every coverage job must carry, likewise from the guide.
#: The requirement is 50 minutes, and this is that plus the margin.
REQUIRED_JOB_CEILING_SECONDS: typ.Final[float] = 65 * 60.0


class TimeoutBudgetError(ValueError):
    """Raised when a configured budget cannot be read as a bound.

    Attributes
    ----------
    field : str
        The configuration key at fault.
    value : object
        What the configuration said, so the message names the text an
        author has to change rather than only the rule it broke.
    """

    def __init__(self, message: str, *, field: str, value: object) -> None:
        """Record the failing key and its value alongside the message.

        Parameters
        ----------
        message : str
            The human-readable explanation.
        field : str
            The configuration key at fault.
        value : object
            What the configuration said.
        """
        super().__init__(message)
        self.field = field
        self.value = value


class NextestConfigurationError(TimeoutBudgetError):
    """Raised when the nextest configuration cannot be read at all.

    Separate from a budget that reads as unbounded. A file that is not
    TOML, a ``slow-timeout`` table with no ``period``, or a file with no
    ``slow-timeout`` anywhere, is a configuration this contract cannot
    reason about rather than one whose tiers are in the wrong order.
    """


class UnboundedTestError(TimeoutBudgetError):
    """Raised when a ``slow-timeout`` terminates no test.

    ``terminate-after`` is optional, and without it nextest marks a test
    slow and lets it run on, so the configuration parses, reads as
    deliberate, and bounds nothing. Reporting that as a period-long
    budget would put a number on the tier that is missing.
    """


def mapping_or_empty(value: object) -> dict[str, object]:
    """Return a YAML value as a mapping, or an empty one.

    Structural matching rather than a chain of `isinstance` checks, per
    this repository's own guidance, and with an explicit fallback so a
    scalar or a list where a mapping was expected yields nothing rather
    than raising from an attribute access several frames away.

    Parameters
    ----------
    value : object
        Any value the YAML parser produced.

    Returns
    -------
    dict[str, object]
        The mapping, or an empty one when the value is not one.
    """
    match value:
        case dict() as mapping:
            return typ.cast("dict[str, object]", mapping)
        case _:
            return {}


def sequence_or_empty(value: object) -> list[object]:
    """Return a YAML value as a list, or an empty one.

    Parameters
    ----------
    value : object
        Any value the YAML parser produced.

    Returns
    -------
    list[object]
        The list, or an empty one when the value is not one.
    """
    match value:
        case list() as items:
            return typ.cast("list[object]", items)
        case _:
            return []


def required_ceiling(budgets: cabc.Sequence[float], allowance: float) -> float:
    """Return the smallest acceptable ceiling for one job, in seconds.

    Three terms. Each coverage step may legitimately spend its whole
    watchdog, so the sum is the floor. The measured work outside those
    windows is added because the job timer covers it and the watchdogs
    do not. The margin is added because a ceiling equal to that sum
    cancels the job at the moment the watchdog would have reported the
    overrun, and the report is the only thing that makes an overrun
    actionable.

    Parameters
    ----------
    budgets : cabc.Sequence[float]
        One watchdog budget per coverage step in the job.
    allowance : float
        The measured work outside those windows, in seconds.

    Returns
    -------
    float
        The smallest acceptable ceiling, in seconds.
    """
    return sum(budgets) + allowance + CEILING_MARGIN_SECONDS
