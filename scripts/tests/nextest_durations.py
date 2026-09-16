"""Reading a nextest duration exactly as ``humantime`` reads it.

Split from ``timeout_budgets`` so neither outgrows the 400-line limit
``AGENTS.md`` sets, and because this is one self-contained translation:
the grammar and the arithmetic of ``humantime`` 2.3.0, the version
``cargo-nextest`` 0.9.122 resolves through ``humantime_serde``.

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
import typing as typ


class DurationGrammarError(ValueError):
    """Raised when text is not a duration ``humantime`` would accept."""


#: The largest value any intermediate may reach, matching the `u64`
#: `humantime` does its arithmetic in.
_U64_MAX: typ.Final[int] = 2**64 - 1

#: Nanoseconds in one second.
_NANOS_PER_SECOND: typ.Final[int] = 1_000_000_000

#: Whitespace as Rust reads it, spelled as character-class content.
#:
#: ``humantime`` skips what ``char::is_whitespace`` accepts, which is the
#: Unicode ``White_Space`` property. Python's ``\s`` is that same set plus
#: U+001C to U+001F, the four information separators, which Rust rejects.
#: Scanning every code point finds those four and nothing else, in either
#: direction, so subtracting them makes the two sets identical. Spelled
#: ``\s`` instead, this reader returns one second for ``"1\x1cs"`` while
#: nextest refuses to load the same configuration. It is written out rather
#: than as ``[^\S\x1c-\x1f]`` because a class cannot nest inside another,
#: and the digit classes here need to carry it.
_SPACE: typ.Final[str] = (
    r"\t\n\v\f\r \x85\xa0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000"
)

#: The characters :data:`_SPACE` matches, for trimming the ends.
_SPACE_CHARS: typ.Final[str] = "".join(
    chr(code)
    for code in (
        *range(0x09, 0x0E),
        0x20,
        0x85,
        0xA0,
        0x1680,
        *range(0x2000, 0x200B),
        0x2028,
        0x2029,
        0x202F,
        0x205F,
        0x3000,
    )
)

#: One value-and-unit component. The fractional part is optional, and
#: ``humantime`` skips whitespace wherever a digit could go: inside a
#: number, so ``"1 0s"`` is ten seconds, and around the decimal point,
#: so ``"1.5m"`` and ``"1 . 5 m"`` are both ninety seconds. A leading
#: point, a trailing point, a second point, a sign and a digit separator
#: are all refused there and so are refused here.
#:
#: The digit classes are spelled ``[0-9]`` rather than ``\d`` because
#: ``humantime`` matches ``'0'..='9'`` and nothing else, while Python's
#: ``\d`` accepts every Unicode decimal digit: ``\d`` would read an
#: Arabic-Indic one as a number that nextest then refuses to load. The
#: whitespace class is spelled out for the mirror-image reason; see
#: :data:`_SPACE`.
_COMPONENT: typ.Final[re.Pattern[str]] = re.compile(
    rf"(?P<whole>[0-9][0-9{_SPACE}]*)"
    rf"(?:\.[{_SPACE}]*(?P<fraction>[0-9][0-9{_SPACE}]*))?"
    rf"(?P<unit>[A-Za-zµ]+)[{_SPACE}]*"
)

#: The one duration ``humantime`` reads without a unit.
#:
#: ``parse_duration`` opens with ``if s == "0" { return Ok(ZERO) }``,
#: compared against the untrimmed string. So ``"0"`` is zero and
#: ``" 0 "`` is not: the special case misses, the parser then finds a
#: number with no unit after it, and that is ``UnknownUnit``. The
#: comparison here is against the untrimmed text for the same reason.
_BARE_ZERO: typ.Final[str] = "0"

#: Every unit spelling ``humantime`` accepts, mapped to a canonical
#: name. Case is not folded: ``m`` is minutes and ``M`` is months, so
#: folding would read a thirty-minute budget as a two-and-a-half-year
#: one.
_UNITS: typ.Final[dict[str, str]] = {
    "nanos": "ns", "nsec": "ns", "ns": "ns",
    "usec": "us", "us": "us", "µs": "us",
    "millis": "ms", "msec": "ms", "ms": "ms",
    "seconds": "s", "second": "s", "secs": "s", "sec": "s", "s": "s",
    "minutes": "m", "minute": "m", "mins": "m", "min": "m", "m": "m",
    "hours": "h", "hour": "h", "hrs": "h", "hr": "h", "h": "h",
    "days": "d", "day": "d", "d": "d",
    "weeks": "w", "week": "w", "wks": "w", "wk": "w", "w": "w",
    "months": "M", "month": "M", "M": "M",
    "years": "y", "year": "y", "yrs": "y", "yr": "y", "y": "y",
}

#: What one whole unit contributes, as (seconds, nanoseconds). Only one
#: of the pair is ever non-zero, which is how ``humantime`` keeps long
#: durations in range: a year is 31,557,600 seconds, never 3.15e16
#: nanoseconds.
_WHOLE: typ.Final[dict[str, tuple[int, int]]] = {
    "ns": (0, 1),
    "us": (0, 1_000),
    "ms": (0, 1_000_000),
    "s": (1, 0),
    "m": (60, 0),
    "h": (3_600, 0),
    "d": (86_400, 0),
    "w": (604_800, 0),
    "M": (2_630_016, 0),
    "y": (31_557_600, 0),
}

#: What a fraction of one unit scales by, and whether the quotient is
#: seconds or nanoseconds. ``ns`` is absent: ``humantime`` refuses a
#: fraction of a nanosecond outright rather than rounding it.
_FRACTION: typ.Final[dict[str, tuple[int, bool]]] = {
    "us": (1_000, False),
    "ms": (1_000_000, False),
    "s": (_NANOS_PER_SECOND, False),
    "m": (60 * _NANOS_PER_SECOND, False),
    "h": (3_600, True),
    "d": (86_400, True),
    "w": (604_800, True),
    "M": (2_630_016, True),
    "y": (31_557_600, True),
}


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
    if value > _U64_MAX:
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
    scale = _FRACTION.get(unit)
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
        A match of `_COMPONENT`.
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
    unit = _UNITS.get(component["unit"])
    if unit is None:
        message = (
            f"nextest duration {duration!r} names the unit "
            f"{component['unit']!r}, which humantime does not accept; note "
            f"that 'm' is minutes and 'M' is months"
        )
        raise DurationGrammarError(message)
    whole = int("".join(component["whole"].split()))
    per_second, per_nano = _WHOLE[unit]
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
    if nanos > _NANOS_PER_SECOND:
        seconds = _in_range(seconds + nanos // _NANOS_PER_SECOND, duration)
        nanos %= _NANOS_PER_SECOND
    total_seconds = _in_range(total_seconds + seconds, duration)
    if nanos >= _NANOS_PER_SECOND:
        total_seconds = _in_range(
            total_seconds + nanos // _NANOS_PER_SECOND, duration
        )
        nanos %= _NANOS_PER_SECOND
    return total_seconds, nanos


def read(duration: str) -> float:
    """Convert a nextest duration to seconds.

    Parameters
    ----------
    duration : str
        A duration as nextest spells it, such as ``"120s"``, the
        multi-component ``"1m 30s"`` or the fractional ``"1.5m"``.

    Returns
    -------
    float
        The duration in seconds.

    Raises
    ------
    DurationGrammarError
        If the text is not a duration nextest would accept.

    Examples
    --------
    >>> read("120s")
    120.0
    >>> read("1m 30s")
    90.0
    >>> read("1.5m")
    90.0
    >>> read("1 0s")
    10.0
    >>> read("0")
    0.0
    """
    if duration == _BARE_ZERO:
        return 0.0
    text = duration.strip(_SPACE_CHARS)
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
        component = _COMPONENT.match(text, position)
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
    return total_seconds + total_nanos / _NANOS_PER_SECOND
