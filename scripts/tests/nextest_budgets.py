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

import tomllib
import typing as typ

from fractions import Fraction

from timeout_budgets import (
    NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS,
    TERMINATION_SAFETY_MARGIN_SECONDS,
    NextestConfigurationError,
    UnboundedTestError,
    mapping_or_empty,
    seconds,
    sequence_or_empty,
)


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


def _table_budget(path: str, table: dict[str, object]) -> Fraction:
    """Return one ``slow-timeout`` table's per-test budget, in seconds.

    Parameters
    ----------
    path : str
        The dotted path of the declaring table, for the message.
    table : dict[str, object]
        The parsed ``slow-timeout`` table.

    Returns
    -------
    Fraction
        The period multiplied by ``terminate-after``, exactly.

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
    return seconds(period) * Fraction(str(multiplier))


def _budget_of(path: str, value: object) -> Fraction:
    """Return the per-test budget one ``slow-timeout`` value declares.

    Parameters
    ----------
    path : str
        The dotted path of the declaring table, for the message.
    value : object
        The parsed value, a table or a bare duration.

    Returns
    -------
    Fraction
        The budget in seconds, exactly.

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


def largest_test_allowance(config_text: str) -> Fraction:
    """Return the longest a single test may run, in seconds.

    nextest warns once per ``period`` and terminates after
    ``terminate-after`` of them, so the budget is their product.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    Fraction
        The longest per-test budget, period multiplied by
        ``terminate-after``, exactly.

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


def _grace_period_of(path: str, value: object) -> Fraction | None:
    """Return the grace period one ``slow-timeout`` value puts in force.

    ``grace-period`` is a field of the ``slow-timeout`` setting rather
    than a setting of its own, and nextest defaults it per declaration:
    a table that omits it gets ten seconds, not the value some other
    table names. A reader that skipped such a table would report five
    seconds for a file whose override actually allows ten.

    Absent and present-but-not-a-string are different states. nextest
    deserializes the field through ``humantime_serde``, which takes a
    string, so ``grace-period = 10`` is a configuration error there and
    the file does not load at all. Folding that into the default would
    let this reader report a budget for a configuration no run could
    use.

    Parameters
    ----------
    path : str
        The dotted path of the declaring table, for the message.
    value : object
        The parsed ``slow-timeout`` value.

    Returns
    -------
    Fraction or None
        The grace period this declaration puts in force, or None when
        the value is not a table and so declares no termination window.

    Raises
    ------
    NextestConfigurationError
        If ``grace-period`` is present and is not a duration string.
    """
    if not isinstance(value, dict):
        return None
    table = typ.cast("dict[str, object]", value)
    match table:
        case {"grace-period": str() as period}:
            return seconds(period)
        case {"grace-period": invalid}:
            message = (
                f"{path}.slow-timeout.grace-period is not a duration string: "
                f"{invalid!r}; nextest reads the field through humantime and "
                f"refuses the configuration"
            )
            raise NextestConfigurationError(
                message, field="grace-period", value=invalid
            )
        case _:
            return NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS


def grace_period(config_text: str) -> Fraction:
    """Return the longest grace period the configuration puts in force.

    Read from the configuration rather than fixed, so a profile that
    raised its grace period raises the requirement too. Every
    ``slow-timeout`` table counts, including one that omits the field:
    nextest gives that table its own ten-second default rather than
    another table's value, so a file naming 5 s in one place and nothing
    in another allows ten seconds, not five.

    Parameters
    ----------
    config_text : str
        The nextest configuration file's text.

    Returns
    -------
    Fraction
        The largest grace period in force, or nextest's default when the
        configuration declares no ``slow-timeout`` table at all.

    Raises
    ------
    NextestConfigurationError
        If a ``slow-timeout`` table names a ``grace-period`` that is not
        a duration string, which nextest itself refuses to load.
    """
    periods = [
        period
        for path, value in _slow_timeouts(_parsed(config_text))
        if (period := _grace_period_of(path, value)) is not None
    ]
    return max(periods, default=NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS)


def termination_allowance(config_text: str) -> Fraction:
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
    Fraction
        The grace period plus the safety margin.
    """
    return grace_period(config_text) + TERMINATION_SAFETY_MARGIN_SECONDS


def global_timeout(config_text: str) -> Fraction | None:
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
    Fraction or None
        The whole-run budget in seconds, or None when the default
        profile declares none.
    """
    profile = mapping_or_empty(
        mapping_or_empty(_parsed(config_text).get("profile")).get("default")
    )
    budget = profile.get("global-timeout")
    return seconds(budget) if isinstance(budget, str) else None
