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

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``, and the canonical wording in
`leynos/shared-actions`' `generate-coverage` README.
"""

import re
import typing as typ
from pathlib import Path

import pytest
import yaml

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

#: The ceiling every coverage job must carry, likewise from the guide.
REQUIRED_JOB_CEILING_SECONDS: typ.Final[float] = 60 * 60.0

_DURATION: typ.Final[re.Pattern[str]] = re.compile(
    r"^\s*(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>ms|s|m|h)\s*$"
)

_UNIT_SECONDS: typ.Final[dict[str, float]] = {
    "ms": 0.001,
    "s": 1.0,
    "m": 60.0,
    "h": 3600.0,
}

#: One `slow-timeout` inline table, captured whole so the period and the
#: multiplier that scales it are read together. nextest warns once per
#: `period` and terminates after `terminate-after` of them, so the budget
#: is their product; reading the period alone understates it fivefold
#: here.
_SLOW_TIMEOUT: typ.Final[re.Pattern[str]] = re.compile(
    r"slow-timeout\s*=\s*\{(?P<body>[^}]*)\}"
)

_GRACE_PERIOD: typ.Final[re.Pattern[str]] = re.compile(r'grace-period\s*=\s*"([^"]+)"')


def seconds(duration: str) -> float:
    """Convert a nextest duration to seconds.

    Parameters
    ----------
    duration : str
        A duration as nextest spells it, such as ``"120s"``.

    Returns
    -------
    float
        The duration in seconds.
    """
    match = _DURATION.match(duration)
    assert match is not None, f"unrecognized nextest duration {duration!r}"
    return float(match["value"]) * _UNIT_SECONDS[match["unit"]]


def largest_test_allowance(config_text: str) -> float:
    """Return the longest a single test may run, in seconds.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    float
        The longest per-test budget, period multiplied by
        ``terminate-after``.
    """
    budgets: list[float] = []
    for match in _SLOW_TIMEOUT.finditer(config_text):
        body = match["body"]
        period = re.search(r'period\s*=\s*"([^"]+)"', body)
        assert period is not None, f"slow-timeout without a period: {body!r}"
        terminate = re.search(r"terminate-after\s*=\s*(\d+)", body)
        multiplier = 1 if terminate is None else int(terminate[1])
        budgets.append(seconds(period[1]) * multiplier)
    assert budgets, "nextest.toml must set at least one slow-timeout"
    return max(budgets)


def grace_period(config_text: str) -> float:
    """Return the longest grace period the configuration names, in seconds.

    Read from the configuration rather than fixed, so a profile that
    raised its grace period raises the requirement too. nextest's own
    default applies only when none is named; this file names 5 s.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    float
        The largest configured grace period, or nextest's default.
    """
    periods = _GRACE_PERIOD.findall(config_text)
    return max(
        (seconds(period) for period in periods),
        default=NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS,
    )


def termination_allowance(config_text: str) -> float:
    """Return the time nextest may take to stop the run, in seconds.

    Two terms, not one: what nextest promises a test after ``SIGTERM``,
    plus a margin for the teardown and report writing that follow it.
    A single floor over the two would make a raised grace period look
    free right up to the run it cancelled, since every value below the
    margin would vanish into it.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    float
        The grace period plus the safety margin.
    """
    return grace_period(config_text) + TERMINATION_SAFETY_MARGIN_SECONDS


def global_timeout(config_text: str) -> float | None:
    """Return the whole-run budget, or None when none is set.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    float or None
        The whole-run budget in seconds, or None when the configuration
        declares none.
    """
    found = re.search(r'^global-timeout\s*=\s*"([^"]+)"', config_text, re.MULTILINE)
    return None if found is None else seconds(found[1])


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
    watchdogs : tuple[float | None, ...]
        The watchdog budget in force for each of those steps, in order,
        with None where neither the step nor the job sets one.
    job_timeout : float or None
        The job's ``timeout-minutes`` in seconds, or None when it
        declares none and so inherits GitHub's six-hour default.
    """

    workflow: str
    job: str
    steps: int
    watchdogs: tuple[float | None, ...]
    job_timeout: float | None

    def __str__(self) -> str:
        """Return a location suitable for a failure message.

        Returns
        -------
        str
            ``workflow:job`` for this job.
        """
        return f"{self.workflow}:{self.job}"


def _mapping(value: object) -> dict[str, object]:
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


def _sequence(value: object) -> list[object]:
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


def _watchdog_of(
    document: dict[str, object],
    job: dict[str, object],
    step: dict[str, object],
) -> float | None:
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
    float or None
        The budget in seconds, or None when no level sets one.
    """
    for owner in (step, job, document):
        raw = _mapping(owner.get("env")).get(WATCHDOG_VARIABLE)
        if raw is not None:
            return float(str(raw))
    return None


def _workflow_documents() -> dict[str, dict[str, object]]:
    """Return every workflow document, keyed by file name.

    Both extensions are read. A coverage lane in the other one would
    otherwise escape every assertion below without failing anything.

    Returns
    -------
    dict[str, dict[str, object]]
        File name to parsed document.
    """
    documents: dict[str, dict[str, object]] = {}
    for pattern in ("*.yml", "*.yaml"):
        for path in sorted(WORKFLOWS_DIRECTORY.glob(pattern)):
            parsed: object = yaml.safe_load(path.read_text(encoding="utf-8"))
            document = _mapping(parsed)
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
    steps = [_mapping(step) for step in _sequence(job.get("steps"))]
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
        job_timeout=None if raw_timeout is None else float(str(raw_timeout)) * 60.0,
    )


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

    Jobs are the unit rather than steps, because the ceiling is a job's
    and it has to contain every watchdog inside it. Counting steps is
    what makes a job with two invocations visible to the arithmetic.

    Returns
    -------
    tuple[CoverageJob, ...]
        One entry per coverage-invoking job.
    """
    return tuple(
        found
        for name, document in _workflow_documents().items()
        for job_name, job in _mapping(document.get("jobs")).items()
        if (found := _coverage_job(name, document, str(job_name), _mapping(job)))
        is not None
    )


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
        required = sum(budgets) + allowance
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
            f"totalling {sum(budgets):.0f}s plus {allowance:.0f}s of measured "
            f"work outside them; an overrun would be cancelled rather than "
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


@pytest.mark.parametrize(
    ("config_text", "expected"),
    [
        pytest.param(
            'slow-timeout = { period = "180s", terminate-after = 1 }',
            180.0,
            id="a-single-period",
        ),
        pytest.param(
            'slow-timeout = { period = "60s", terminate-after = 5 }',
            300.0,
            id="five-warning-periods",
        ),
        pytest.param(
            'slow-timeout = { period = "2m", terminate-after = 3 }',
            360.0,
            id="minutes-times-three",
        ),
        pytest.param(
            'slow-timeout = { period = "90s" }',
            90.0,
            id="no-multiplier-means-one",
        ),
        pytest.param(
            'slow-timeout = { period = "30s", terminate-after = 2, '
            'grace-period = "5s" }\n'
            'slow-timeout = { period = "60s", terminate-after = 1 }',
            60.0,
            id="the-largest-of-several",
        ),
    ],
)
def test_the_largest_per_test_allowance_counts_the_multiplier(
    config_text: str, expected: float
) -> None:
    """``terminate-after`` scales the period; the budget is their product.

    This is the reading every comparison above rests on, and it is the
    one easy to get wrong. It is driven with controlled configurations
    rather than this repository's own, whose multipliers are all one:
    against that file a reading that ignored the multiplier entirely
    would give the same answer, and the test would prove nothing.
    """
    assert largest_test_allowance(config_text) == pytest.approx(expected), (
        f"{config_text!r} must yield a {expected:.0f}s largest per-test "
        f"allowance; terminate-after scales the period"
    )


def test_a_grace_period_is_not_read_as_a_per_test_budget() -> None:
    """The two keys sit in the same inline table.

    A matcher reading `period` as a substring would take a grace period
    for a per-test budget whenever the former were the larger, which
    would silently raise the whole-run budget this contract demands.
    """
    config_text = 'slow-timeout = { period = "30s", grace-period = "30m" }'
    assert largest_test_allowance(config_text) == pytest.approx(30.0), (
        "the per-test ceiling read a grace period as a slow-timeout"
    )


def test_the_termination_allowance_is_the_grace_period_plus_the_margin() -> None:
    """The two terms are added, not maximized over.

    A single floor over the grace period and the margin would absorb
    every grace period below the margin, so raising this file's five
    seconds to thirty would demand nothing more of the watchdog above
    it. The ordering assertions above cannot see the difference, since
    both readings leave the requirement well inside 1,800 s, which is
    exactly why the reading needs a test of its own.
    """
    unset = termination_allowance("")
    assert unset == pytest.approx(
        NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS + TERMINATION_SAFETY_MARGIN_SECONDS
    ), "an unset grace period must fall back to nextest's own default"
    configured = termination_allowance(
        'slow-timeout = { period = "30s", grace-period = "5s" }'
    )
    assert configured == pytest.approx(5.0 + TERMINATION_SAFETY_MARGIN_SECONDS), (
        "a grace period below the margin must still raise the allowance; "
        "a maximum over the two terms would have discarded it"
    )
    largest = termination_allowance(
        'slow-timeout = { grace-period = "5s" }\nslow-timeout = { grace-period = "45s" }'
    )
    assert largest == pytest.approx(45.0 + TERMINATION_SAFETY_MARGIN_SECONDS), (
        "the largest configured grace period governs the allowance"
    )
