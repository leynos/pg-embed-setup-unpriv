"""Reading `.config/nextest.toml` as the runner reads it.

The configuration is parsed as TOML rather than scraped. A text match
finds a ``slow-timeout`` inside a comment, inside a ``filter`` string,
or in a table nextest never consults; a commented-out ``global-timeout``
would keep reading as the budget in force long after the tier had been
switched off.

The reading that matters most is the per-test one. nextest warns once
per ``period`` and terminates after ``terminate-after`` of them, so the
budget is their product, and a ``slow-timeout`` naming no
``terminate-after`` never terminates anything at all. That form is
refused rather than read as a single period.
"""

import re
import tomllib
import typing as typ

from timeout_budgets import (
    NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS,
    TERMINATION_SAFETY_MARGIN_SECONDS,
    NextestConfigurationError,
    UnboundedTestError,
    mapping_or_empty,
    sequence_or_empty,
)

_DURATION: typ.Final[re.Pattern[str]] = re.compile(
    r"^\s*(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>ms|s|m|h)\s*$"
)

_UNIT_SECONDS: typ.Final[dict[str, float]] = {
    "ms": 0.001,
    "s": 1.0,
    "m": 60.0,
    "h": 3600.0,
}


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


def _parsed(config_text: str) -> dict[str, object]:
    """Return the nextest configuration as TOML.

    Parsing rather than scraping is the whole point of this reader. A
    text match finds a ``slow-timeout`` inside a comment, inside a
    ``filter`` string, or in a table nextest never consults, and reports
    a budget the runner does not use. A commented-out
    ``global-timeout`` is the case that matters most here: the guide and
    this contract would both go on recording a tier that had been
    switched off.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    dict[str, object]
        The parsed document.

    Raises
    ------
    NextestConfigurationError
        If the text is not valid TOML.
    """
    try:
        return tomllib.loads(config_text)
    except tomllib.TOMLDecodeError as error:
        message = f"the nextest configuration is not valid TOML: {error}"
        raise NextestConfigurationError(
            message, field="config", value=config_text
        ) from error


def _profile_tables(config: dict[str, object]) -> list[tuple[str, dict[str, object]]]:
    """Return every table nextest reads a per-test budget from.

    A profile's own table and each of its ``[[overrides]]`` entries,
    each with the path that names it, so a failure can say which one is
    at fault.

    Parameters
    ----------
    config : dict[str, object]
        The parsed configuration.

    Returns
    -------
    list of tuple
        The dotted path and the table, in file order.
    """
    tables: list[tuple[str, dict[str, object]]] = []
    for name, raw_profile in mapping_or_empty(config.get("profile")).items():
        profile = mapping_or_empty(raw_profile)
        tables.append((f"profile.{name}", profile))
        tables.extend(
            (f"profile.{name}.overrides[{index}]", mapping_or_empty(override))
            for index, override in enumerate(
                sequence_or_empty(profile.get("overrides"))
            )
        )
    return tables


def _slow_timeouts(config: dict[str, object]) -> list[tuple[str, object]]:
    """Return every ``slow-timeout`` the configuration declares.

    Parameters
    ----------
    config : dict[str, object]
        The parsed configuration.

    Returns
    -------
    list of tuple
        The dotted path of the declaring table and the value.
    """
    return [
        (path, table["slow-timeout"])
        for path, table in _profile_tables(config)
        if "slow-timeout" in table
    ]


def _table_budget(path: str, table: dict[str, object]) -> float:
    """Return one ``slow-timeout`` table's per-test budget, in seconds.

    Parameters
    ----------
    path : str
        The dotted path of the declaring table, for the message.
    table : dict[str, object]
        The parsed ``slow-timeout`` table.

    Returns
    -------
    float
        The period multiplied by ``terminate-after``.

    Raises
    ------
    NextestConfigurationError
        If the table names no ``period``.
    UnboundedTestError
        If the table names no ``terminate-after``, so nextest warns
        about a slow test forever and never stops it.
    """
    period = table.get("period")
    if not isinstance(period, str):
        message = f"{path}.slow-timeout names no period: {table!r}"
        raise NextestConfigurationError(message, field="period", value=table)
    multiplier = table.get("terminate-after")
    if multiplier is None:
        message = (
            f"{path}.slow-timeout sets no terminate-after, so nextest marks "
            f"the test slow and lets it run on; there is no per-test tier to "
            f"compare against"
        )
        raise UnboundedTestError(message, field="terminate-after", value=table)
    return seconds(period) * float(str(multiplier))


def _budget_of(path: str, value: object) -> float:
    """Return the per-test budget one ``slow-timeout`` value declares.

    Parameters
    ----------
    path : str
        The dotted path of the declaring table, for the message.
    value : object
        The parsed value, a table or a bare duration.

    Returns
    -------
    float
        The budget in seconds.

    Raises
    ------
    UnboundedTestError
        If the value is a bare duration. That form sets a warning period
        with no ``terminate-after``, so no test is ever terminated by it.
    NextestConfigurationError
        If the value is neither a table nor a string.
    """
    match value:
        case dict() as table:
            return _table_budget(path, typ.cast("dict[str, object]", table))
        case str() as period:
            message = (
                f'{path}.slow-timeout = "{period}" sets a warning period with '
                f"no terminate-after, so nextest reports the test as slow and "
                f"never stops it; there is no per-test tier to compare against"
            )
            raise UnboundedTestError(message, field="slow-timeout", value=period)
        case _:
            message = f"{path}.slow-timeout is neither a table nor a duration"
            raise NextestConfigurationError(message, field="slow-timeout", value=value)


def largest_test_allowance(config_text: str) -> float:
    """Return the longest a single test may run, in seconds.

    nextest warns once per ``period`` and terminates after
    ``terminate-after`` of them, so the budget is their product.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    float
        The longest per-test budget, period multiplied by
        ``terminate-after``.

    Raises
    ------
    NextestConfigurationError
        If the configuration declares no ``slow-timeout`` at all.
    UnboundedTestError
        If any ``slow-timeout`` terminates no test.
    """
    config = _parsed(config_text)
    budgets = [_budget_of(path, value) for path, value in _slow_timeouts(config)]
    if not budgets:
        message = (
            "the nextest configuration declares no slow-timeout, so no test "
            "is bounded and there is no per-test tier to compare against"
        )
        raise NextestConfigurationError(
            message, field="slow-timeout", value=config_text
        )
    return max(budgets)


def bounds_a_single_test(config_text: str, profile: str = "default") -> bool:
    """Return whether a profile's own table terminates a slow test.

    Only the profile's own ``slow-timeout`` counts. An override bounds
    the tests its filter matches; the profile's own bounds the rest, so
    a profile whose only ``terminate-after`` sits in an override leaves
    every unmatched test running with no bound at all while
    :func:`largest_test_allowance` still reports a comfortable number.

    Parameters
    ----------
    config_text
        The nextest configuration file's text.
    profile
        The profile to read.

    Returns
    -------
    bool
        True when that profile's own ``slow-timeout`` is a table setting
        ``terminate-after``.
    """
    profiles = mapping_or_empty(_parsed(config_text).get("profile"))
    own = mapping_or_empty(profiles.get(profile))
    table = own.get("slow-timeout")
    return isinstance(table, dict) and table.get("terminate-after") is not None


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
    periods = [
        seconds(period)
        for _, value in _slow_timeouts(_parsed(config_text))
        if isinstance(value, dict)
        and isinstance(period := value.get("grace-period"), str)
    ]
    return max(periods, default=NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS)


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

    Read from ``[profile.default]`` alone. nextest's other profiles
    inherit that table unless they override it, and this repository runs
    the default profile, so a value elsewhere is not the budget in
    force. An ``[[overrides]]`` entry cannot carry one at all, and a
    reader that took one from there would report a tier nextest ignores.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    float or None
        The whole-run budget in seconds, or None when the default
        profile declares none.
    """
    profile = mapping_or_empty(
        mapping_or_empty(_parsed(config_text).get("profile")).get("default")
    )
    budget = profile.get("global-timeout")
    return seconds(budget) if isinstance(budget, str) else None
