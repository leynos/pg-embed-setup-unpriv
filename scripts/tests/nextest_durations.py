"""Reading a nextest duration exactly as ``humantime`` reads it.

Split from ``timeout_budgets`` so neither outgrows the 400-line limit
``AGENTS.md`` sets, and because this is one self-contained translation:
the grammar and the arithmetic of ``humantime`` 2.3.0, the version
``cargo-nextest`` 0.9.122 resolves through ``humantime_serde``. The
grammar itself lives in ``nextest_duration_grammar``; this module is the
arithmetic that applies it.

The arithmetic uses integers, and splits into seconds and nanoseconds
the way ``humantime``'s does because reading a component as a float
accepts two classes of text it refuses.

Sub-nanosecond precision is one. ``humantime`` scales a fraction's
numerator by its unit and divides by the denominator, and that division
errors unless it is exact, so ``"0.0000000002s"`` is an error there and
a small positive number to ``float``. Out-of-range values are the
other: every step is checked against the range of a ``u64``, the
denominator included, so twenty fractional digits overflow whatever the
numerator is.

The split matters as much as the integers. ``humantime`` counts whole
hours, days, weeks, months, and years in *seconds*, so a duration of
several centuries is fine there while a single nanosecond accumulator
would overflow and refuse it.
"""

import re
from fractions import Fraction

from nextest_duration_grammar import (
    BARE_ZERO,
    COMPONENT,
    FRACTION,
    NANOS_PER_SECOND,
    SPACE_CHARS,
    U64_MAX,
    UNITS,
    WHOLE,
    DurationGrammarError,
)

__all__ = ["DurationGrammarError", "read_exact"]

def _in_range(value: int, duration: str) -> int:
    """Return `value`, or raise when it leaves the range of a `u64`.

    Parameters
    ----------
    value : int
        The intermediate to check.
    duration : str
        The whole duration, for the message.

    Returns
    -------
    int
        `value` unchanged.

    Raises
    ------
    DurationGrammarError
        If `value` is outside the range `humantime` computes in.
    """
    if value > U64_MAX:
        message = (
            f"nextest duration {duration!r} overflows the range humantime "
            f"computes in; it is not a duration the runner can hold"
        )
        raise DurationGrammarError(message)
    return value


def _exact_division(scaled: int, denominator: int, duration: str) -> int:
    """Return `scaled // denominator`, or raise when it is not exact.

    `humantime`'s division errors on any remainder, which is what makes
    sub-nanosecond precision an error rather than a rounding.

    Parameters
    ----------
    scaled : int
        The numerator scaled by its unit.
    denominator : int
        The fraction's denominator.
    duration : str
        The whole duration, for the message.

    Returns
    -------
    int
        The exact quotient.

    Raises
    ------
    DurationGrammarError
        If the division leaves a remainder.
    """
    if scaled % denominator:
        message = (
            f"nextest duration {duration!r} names a value finer than a "
            f"nanosecond, which humantime refuses rather than rounds"
        )
        raise DurationGrammarError(message)
    return scaled // denominator


def _fraction_of(unit: str, digits: str, duration: str) -> tuple[int, int]:
    """Return one fraction's contribution as (seconds, nanoseconds).

    Parameters
    ----------
    unit : str
        The canonical unit name.
    digits : str
        The digits after the point, whitespace already removed.
    duration : str
        The whole duration, for the message.

    Returns
    -------
    tuple[int, int]
        Seconds and nanoseconds contributed.

    Raises
    ------
    DurationGrammarError
        If the unit takes no fraction, or the division is not exact.
    """
    scale = FRACTION.get(unit)
    if scale is None:
        message = (
            f"nextest duration {duration!r} names a fraction of a nanosecond, "
            f"which humantime refuses"
        )
        raise DurationGrammarError(message)
    numerator = int(digits)
    denominator = _in_range(10 ** len(digits), duration)
    factor, is_seconds = scale
    quotient = _exact_division(
        _in_range(numerator * factor, duration), denominator, duration
    )
    return (quotient, 0) if is_seconds else (0, quotient)


def _component_contributions(
    component: re.Match[str], duration: str
) -> list[tuple[int, int]]:
    """Return one component's contributions, whole part before fraction.

    Two contributions rather than one sum, in that order, because
    ``humantime`` adds them to the running total separately and the
    running total is normalized between the two. Summing them here and
    adding once would accept totals it refuses and refuse totals it
    accepts.

    Parameters
    ----------
    component : re.Match[str]
        A match of `COMPONENT`.
    duration : str
        The whole duration, for the message.

    Returns
    -------
    list[tuple[int, int]]
        One or two (seconds, nanoseconds) pairs, in the order
        `humantime` adds them.

    Raises
    ------
    DurationGrammarError
        If the unit is unknown, or the value is out of range or finer
        than `humantime` allows.
    """
    unit = UNITS.get(component["unit"])
    if unit is None:
        message = (
            f"nextest duration {duration!r} names the unit "
            f"{component['unit']!r}, which humantime does not accept; note "
            f"that 'm' is minutes and 'M' is months"
        )
        raise DurationGrammarError(message)
    whole = int("".join(component["whole"].split()))
    per_second, per_nano = WHOLE[unit]
    contributions = [
        (
            _in_range(whole * per_second, duration),
            _in_range(whole * per_nano, duration),
        )
    ]
    raw_fraction = component["fraction"]
    if raw_fraction is not None:
        contributions.append(
            _fraction_of(unit, "".join(raw_fraction.split()), duration)
        )
    return contributions


def _add_current(
    total: tuple[int, int], contribution: tuple[int, int], duration: str
) -> tuple[int, int]:
    """Add one contribution to the running total the way `humantime` does.

    `humantime` keeps whole seconds and a nanosecond part, both `u64`,
    and normalizes after every contribution rather than at the end. That
    is the whole of the difference between this and a single nanosecond
    accumulator: `"18446744073709551615ns 1ns"` is a fraction over
    eighteen seconds short of nineteen billion, which `humantime` reads,
    and which one accumulator refuses because the two values together
    pass the `u64` ceiling before anything carries.

    The carry is deliberately in two parts, as `humantime`'s is. Its own
    normalization runs only when the nanosecond part is *above* a
    second, so a part of exactly one second reaches `Duration::new`,
    which carries it and aborts if that carry overflows. Both halves
    check, so the abort is a refusal here.

    Parameters
    ----------
    total : tuple[int, int]
        The running (seconds, nanoseconds).
    contribution : tuple[int, int]
        What this whole part or fraction adds.
    duration : str
        The whole duration, for the message.

    Returns
    -------
    tuple[int, int]
        The new running total, its nanosecond part below one second.

    Raises
    ------
    DurationGrammarError
        If any sum leaves the range `humantime` computes in.
    """
    total_seconds, total_nanos = total
    seconds, nanos = contribution
    nanos = _in_range(total_nanos + nanos, duration)
    if nanos > NANOS_PER_SECOND:
        seconds = _in_range(seconds + nanos // NANOS_PER_SECOND, duration)
        nanos %= NANOS_PER_SECOND
    total_seconds = _in_range(total_seconds + seconds, duration)
    if nanos >= NANOS_PER_SECOND:
        total_seconds = _in_range(
            total_seconds + nanos // NANOS_PER_SECOND, duration
        )
        nanos %= NANOS_PER_SECOND
    return total_seconds, nanos


def read_exact(duration: str) -> Fraction:
    """Convert a nextest duration to seconds.

    Parameters
    ----------
    duration : str
        A duration as nextest spells it, such as ``"120s"``, the
        multi-component ``"1m 30s"`` or the fractional ``"1.5m"``.

    Returns
    -------
    Fraction
        The duration in seconds, exactly. A ``Fraction`` is returned
        rather than a ``float`` because the seconds and nanoseconds are
        accumulated as integers and a float discards that exactness
        above two to the fifty-third.

    Raises
    ------
    DurationGrammarError
        If the text is not a duration nextest would accept.

    Examples
    --------
    >>> read_exact("120s") == 120
    True
    >>> read_exact("1m 30s") == 90
    True
    >>> read_exact("1.5m") == 90
    True
    >>> read_exact("1 0s") == 10
    True
    >>> read_exact("0") == 0
    True
    """
    if duration == BARE_ZERO:
        return Fraction(0)
    text = duration.strip(SPACE_CHARS)
    if not text:
        message = (
            f"unrecognized nextest duration {duration!r}; nextest reads "
            f"durations with humantime, which wants a sequence of values "
            f'each carrying a unit, such as "120s", "1m 30s" or "1.5m"'
        )
        raise DurationGrammarError(message)
    total = (0, 0)
    position = 0
    while position < len(text):
        component = COMPONENT.match(text, position)
        if component is None:
            message = (
                f"unrecognized nextest duration {duration!r}; nextest reads "
                f"durations with humantime, which wants a sequence of values "
                f'each carrying a unit, such as "120s", "1m 30s" or "1.5m"'
            )
            raise DurationGrammarError(message)
        for contribution in _component_contributions(component, duration):
            total = _add_current(total, contribution, duration)
        position = component.end()
    total_seconds, total_nanos = total
    return Fraction(total_seconds) + Fraction(total_nanos, NANOS_PER_SECOND)
