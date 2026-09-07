"""How the timeout contract reads the files it compares.

Every assertion in ``test_timeout_ordering_contract.py`` rests on
turning three files into comparable seconds. Those readings can be wrong
while no file is wrong, and this repository's own configuration cannot
expose most of the ways they can be: every ``terminate-after`` here is
one, so a reader that ignored the multiplier entirely would give the
same answer, and nothing here is commented out. So the readings are
driven with controlled configurations.
"""

import pytest
from nextest_budgets import (
    global_timeout,
    grace_period,
    largest_test_allowance,
    termination_allowance,
)
from timeout_budgets import (
    CEILING_MARGIN_SECONDS,
    NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS,
    TERMINATION_SAFETY_MARGIN_SECONDS,
    NextestConfigurationError,
    UnboundedTestError,
    required_ceiling,
)


def _profile(*lines: str) -> str:
    """Return a default profile declaring the given keys.

    Parameters
    ----------
    *lines : str
        Lines to put inside ``[profile.default]``.

    Returns
    -------
    str
        A configuration document.
    """
    return "\n".join(("[profile.default]", *lines)) + "\n"


@pytest.mark.parametrize(
    ("config_text", "expected"),
    [
        pytest.param(
            _profile('slow-timeout = { period = "180s", terminate-after = 1 }'),
            180.0,
            id="a-single-period",
        ),
        pytest.param(
            _profile('slow-timeout = { period = "60s", terminate-after = 5 }'),
            300.0,
            id="five-warning-periods",
        ),
        pytest.param(
            _profile('slow-timeout = { period = "2m", terminate-after = 3 }'),
            360.0,
            id="minutes-times-three",
        ),
        pytest.param(
            _profile(
                'slow-timeout = { period = "30s", terminate-after = 2, '
                'grace-period = "5s" }',
                "",
                "[[profile.default.overrides]]",
                'filter = "binary(=ui)"',
                'slow-timeout = { period = "60s", terminate-after = 1 }',
            ),
            60.0,
            id="the-largest-of-several",
        ),
        pytest.param(
            _profile(
                'slow-timeout = { period = "30s", terminate-after = 1 }',
                "",
                "[[profile.default.overrides]]",
                'filter = "binary(=slow_timeout_probe)"',
                'slow-timeout = { period = "45s", terminate-after = 1 }',
            ),
            45.0,
            id="a-filter-naming-the-key-is-not-a-budget",
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


@pytest.mark.parametrize(
    "config_text",
    [
        pytest.param(_profile('slow-timeout = "2m"'), id="a-bare-duration"),
        pytest.param(
            _profile('slow-timeout = { period = "2m" }'),
            id="a-table-without-terminate-after",
        ),
        pytest.param(
            _profile('slow-timeout = { period = "2m", grace-period = "5s" }'),
            id="a-table-with-only-a-grace-period",
        ),
    ],
)
def test_a_slow_timeout_that_never_terminates_is_refused(config_text: str) -> None:
    """``terminate-after`` is optional, and without it nothing is bounded.

    nextest marks the test slow, warns once per period, and lets it run
    on. Reading such a configuration as a period-long budget would put a
    number on the tier that is missing, and every comparison above it
    would then pass against a tier that does not exist. Nothing in
    ``.config/nextest.toml`` relies on the looser reading: every table
    there sets ``terminate-after`` explicitly.
    """
    with pytest.raises(UnboundedTestError, match=r"terminate-after"):
        largest_test_allowance(config_text)


def test_a_commented_out_slow_timeout_is_not_a_budget() -> None:
    """A comment is not configuration, and TOML is what says so.

    A reader that scraped the file's text would find the commented
    ``slow-timeout`` and report a 30 s allowance from a line nextest
    never reads, so deleting the live entry and leaving the comment
    behind would look like a change of value rather than the loss of a
    tier.
    """
    config_text = _profile(
        '# slow-timeout = { period = "30s", terminate-after = 1 }',
        'slow-timeout = { period = "180s", terminate-after = 1 }',
    )
    assert largest_test_allowance(config_text) == pytest.approx(180.0), (
        "a commented-out slow-timeout was read as a live one"
    )
    with pytest.raises(NextestConfigurationError, match=r"no slow-timeout"):
        largest_test_allowance(
            _profile('# slow-timeout = { period = "30s", terminate-after = 1 }')
        )


def test_a_commented_out_grace_period_is_not_in_force() -> None:
    """The same for the grace period, which sets the watchdog's floor.

    A scraped comment would raise the termination allowance and with it
    the watchdog this contract demands, so the file would appear to ask
    more of the tier above it than nextest actually does.
    """
    config_text = _profile(
        '# slow-timeout = { period = "30s", terminate-after = 1, '
        'grace-period = "30m" }',
        'slow-timeout = { period = "180s", terminate-after = 1, grace-period = "5s" }',
    )
    assert grace_period(config_text) == pytest.approx(5.0), (
        "a commented-out grace period was read as the one in force"
    )
    assert termination_allowance(config_text) == pytest.approx(
        5.0 + TERMINATION_SAFETY_MARGIN_SECONDS
    )


def test_a_commented_out_global_timeout_is_absent() -> None:
    """Tier two must read as missing when it has been switched off.

    This is the reading the contract's presence assertion rests on. A
    text match would keep reporting the budget from the comment, so the
    tier could be commented out and the four-tier contract would go on
    passing with three.
    """
    assert global_timeout(_profile('# global-timeout = "10m"')) is None, (
        "a commented-out global-timeout was read as the budget in force"
    )
    assert global_timeout(_profile('global-timeout = "10m"')) == pytest.approx(600.0)


def test_the_whole_run_budget_is_read_from_the_default_profile() -> None:
    """``global-timeout`` is a profile key, not an override one.

    nextest ignores a ``global-timeout`` inside an ``[[overrides]]``
    entry, so a reader that took one from there would report a tier that
    does not exist.
    """
    config_text = _profile(
        'slow-timeout = { period = "30s", terminate-after = 1 }',
        "",
        "[[profile.default.overrides]]",
        'filter = "binary(=ui)"',
        'global-timeout = "90m"',
    )
    assert global_timeout(config_text) is None


def test_a_grace_period_is_not_read_as_a_per_test_budget() -> None:
    """The two keys sit in the same inline table.

    A matcher reading `period` as a substring would take a grace period
    for a per-test budget whenever the former were the larger, which
    would silently raise the whole-run budget this contract demands.
    """
    config_text = _profile(
        'slow-timeout = { period = "30s", terminate-after = 1, grace-period = "30m" }'
    )
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
    unset = termination_allowance(
        _profile('slow-timeout = { period = "30s", terminate-after = 1 }')
    )
    assert unset == pytest.approx(
        NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS + TERMINATION_SAFETY_MARGIN_SECONDS
    ), "an unset grace period must fall back to nextest's own default"
    configured = termination_allowance(
        _profile(
            'slow-timeout = { period = "30s", terminate-after = 1, '
            'grace-period = "5s" }'
        )
    )
    assert configured == pytest.approx(5.0 + TERMINATION_SAFETY_MARGIN_SECONDS), (
        "a grace period below the margin must still raise the allowance; "
        "a maximum over the two terms would have discarded it"
    )
    largest = termination_allowance(
        _profile(
            'slow-timeout = { period = "30s", terminate-after = 1, '
            'grace-period = "5s" }',
            "",
            "[[profile.default.overrides]]",
            'filter = "binary(=ui)"',
            'slow-timeout = { period = "30s", terminate-after = 1, '
            'grace-period = "45s" }',
        )
    )
    assert largest == pytest.approx(45.0 + TERMINATION_SAFETY_MARGIN_SECONDS), (
        "the largest configured grace period governs the allowance"
    )


def test_the_required_ceiling_carries_all_three_terms() -> None:
    """Watchdogs, measured work outside them, and the margin.

    Both ceilings clear the smaller requirement too, so dropping the
    margin changes nothing the assertion over the workflows can see.
    Driving the derivation with controlled numbers is what makes the
    missing term visible.
    """
    assert required_ceiling([1800.0, 2700.0], 1200.0) == pytest.approx(
        4500.0 + 1200.0 + CEILING_MARGIN_SECONDS
    ), "two watchdogs, the allowance and the margin are all added"
    assert required_ceiling([1800.0], 0.0) == pytest.approx(
        1800.0 + CEILING_MARGIN_SECONDS
    ), "the margin applies even when nothing runs outside the watchdog"
    assert required_ceiling([], 0.0) == pytest.approx(CEILING_MARGIN_SECONDS), (
        "the margin is a term of its own, not a fraction of the others"
    )
