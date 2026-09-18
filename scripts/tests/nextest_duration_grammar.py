"""The grammar ``humantime`` 2.3.0 accepts, and the units it scales by.

Split from ``nextest_durations`` so neither file outgrows the 400-line
limit ``AGENTS.md`` sets, and because the two halves are different kinds
of statement: this one is a transcription of ``humantime``'s tables and
patterns, the other is the checked arithmetic that applies them. A unit
spelling or a digit class changes here; a carry or an overflow check
changes there.

``cargo-nextest`` 0.9.122 resolves ``humantime`` 2.3.0 through
``humantime_serde``, so these are the spellings a nextest configuration
may use and the only ones it may use.
"""

import re
import typing as typ

class DurationGrammarError(ValueError):
    """Raised when text is not a duration ``humantime`` would accept."""


#: The largest value any intermediate may reach, matching the `u64`
#: `humantime` does its arithmetic in.
U64_MAX: typ.Final[int] = 2**64 - 1

#: Nanoseconds in one second.
NANOS_PER_SECOND: typ.Final[int] = 1_000_000_000

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
SPACE: typ.Final[str] = (
    r"\t\n\v\f\r \x85\xa0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000"
)

#: The characters :data:`SPACE` matches, for trimming the ends.
SPACE_CHARS: typ.Final[str] = "".join(
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
#: :data:`SPACE`.
COMPONENT: typ.Final[re.Pattern[str]] = re.compile(
    rf"(?P<whole>[0-9][0-9{SPACE}]*)"
    rf"(?:\.[{SPACE}]*(?P<fraction>[0-9][0-9{SPACE}]*))?"
    rf"(?P<unit>[A-Za-zµ]+)[{SPACE}]*"
)

#: The one duration ``humantime`` reads without a unit.
#:
#: ``parse_duration`` opens with ``if s == "0" { return Ok(ZERO) }``,
#: compared against the untrimmed string. So ``"0"`` is zero and
#: ``" 0 "`` is not: the special case misses, the parser then finds a
#: number with no unit after it, and that is ``UnknownUnit``. The
#: comparison here is against the untrimmed text for the same reason.
BARE_ZERO: typ.Final[str] = "0"

#: Every unit spelling ``humantime`` accepts, mapped to a canonical
#: name. Case is not folded: ``m`` is minutes and ``M`` is months, so
#: folding would read a thirty-minute budget as a two-and-a-half-year
#: one.
UNITS: typ.Final[dict[str, str]] = {
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
WHOLE: typ.Final[dict[str, tuple[int, int]]] = {
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
FRACTION: typ.Final[dict[str, tuple[int, bool]]] = {
    "us": (1_000, False),
    "ms": (1_000_000, False),
    "s": (NANOS_PER_SECOND, False),
    "m": (60 * NANOS_PER_SECOND, False),
    "h": (3_600, True),
    "d": (86_400, True),
    "w": (604_800, True),
    "M": (2_630_016, True),
    "y": (31_557_600, True),
}
