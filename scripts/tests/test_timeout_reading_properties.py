"""Properties the timeout readings hold for inputs nobody wrote down.

Split from ``test_timeout_reading_contract.py``, which the parametrised
cases outgrew against the 400-line limit ``AGENTS.md`` sets, and because
these two tests answer a different question. The parametrised cases pin
the spellings this repository's files actually use. These generate the
ones they do not: a duration of several components, and a
``terminate-after`` other than the 1 every table here sets. Both are
readings that could be wrong while every file in the repository is
right, so no configuration under version control can expose them.
"""

import typing as typ

import pytest
from hypothesis import given
from hypothesis import strategies as st
from nextest_budgets import largest_test_allowance
from timeout_budgets import seconds

#: Unit spellings whose length in seconds is an exact integer, paired
#: with that length. Sub-second units are left out so the expected sum
#: stays exact and a failure means the reader is wrong rather than the
#: arithmetic being imprecise.
_EXACT_UNITS: typ.Final[dict[str, int]] = {
    "s": 1,
    "sec": 1,
    "seconds": 1,
    "m": 60,
    "min": 60,
    "minutes": 60,
    "h": 3600,
    "hr": 3600,
    "hours": 3600,
    "d": 86400,
    "day": 86400,
    "w": 604800,
    "wk": 604800,
    "M": 2630016,
    "y": 31557600,
    "yr": 31557600,
}

#: One component: a small whole value and one of those units.
_COMPONENTS: typ.Final[st.SearchStrategy[tuple[int, str]]] = st.tuples(
    st.integers(min_value=0, max_value=999),
    st.sampled_from(sorted(_EXACT_UNITS)),
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
        The configuration text.
    """
    body = "\n".join(lines)
    return f"[profile.default]\n{body}\n"


@given(
    components=st.lists(_COMPONENTS, min_size=1, max_size=6),
    separators=st.lists(st.sampled_from(["", " ", "  "]), min_size=6, max_size=6),
)
def test_a_multi_component_duration_reads_as_the_sum_of_its_components(
    components: list[tuple[int, str]],
    separators: list[str],
) -> None:
    """`humantime` sums a duration's components, and so must this reader.

    The repository's own configuration has one component per duration,
    so a reader that took the first component and stopped, or that
    multiplied instead of summing, would give the same answers on every
    file this contract compares. The expected total is built from a unit
    table written out here rather than imported, so the test would still
    fail if `_UNIT_SECONDS` were edited to agree with a broken reader.

    Joining with and without spaces exercises the same grammar
    `1m30s` and `1m 30s` both use.
    """
    rendered = "".join(
        f"{value}{unit}{separators[index % len(separators)]}"
        for index, (value, unit) in enumerate(components)
    )
    expected = sum(value * _EXACT_UNITS[unit] for value, unit in components)
    assert seconds(rendered) == pytest.approx(float(expected)), (
        f"{rendered!r} must read as {expected} seconds"
    )


@given(
    period=st.integers(min_value=1, max_value=86_400),
    multiplier=st.integers(min_value=1, max_value=99),
)
def test_a_per_test_budget_is_the_period_times_the_terminate_after(
    period: int,
    multiplier: int,
) -> None:
    """nextest stops a test after `terminate-after` periods, not one.

    Every `terminate-after` in this repository is 1, so a reader that
    ignored the multiplier outright would agree with the configuration
    on every value it currently holds and would understate the tier the
    moment anyone raised one. Generating the pair is what separates the
    two readings.
    """
    config = _profile(
        f'slow-timeout = {{ period = "{period}s", terminate-after = {multiplier} }}'
    )
    assert largest_test_allowance(config) == pytest.approx(
        float(period * multiplier)
    ), f"a {period}s period terminating after {multiplier} bounds a test at their product"
