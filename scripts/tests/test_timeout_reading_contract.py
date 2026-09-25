"""How the timeout contract reads the files it compares.

Every assertion in ``test_timeout_ordering_contract.py`` rests on
turning three files into comparable seconds. Those readings can be wrong
while no file is wrong, and this repository's own configuration cannot
expose most of the ways they can be: every ``terminate-after`` here is
one, so a reader that ignored the multiplier entirely would give the
same answer, and nothing here is commented out. So the readings are
driven with controlled configurations.

Every expectation is an exact ``Fraction`` compared with ``==`` rather
than a float compared with ``pytest.approx``. The readings are exact, so
an approximate expectation asks less of them than they promise, and at
the ``u64::MAX`` boundary it asks almost nothing. Neighbouring floats
are 2,048 apart around ``18446744073709551615``, so the nanosecond part
of that case vanishes on conversion, and ``approx``'s relative tolerance
of one part in a million admits an error of some eighteen million
million seconds besides. A boundary-arithmetic regression of any
plausible size would pass.
"""

import pathlib

from fractions import Fraction

import pytest
from coverage_lanes import (
    WorkflowReadError,
    WorkflowValueError,
    coverage_jobs_in,
    load_workflow_documents,
)
from nextest_budgets import (
    global_timeout,
    grace_period,
    largest_test_allowance,
    termination_allowance,
)
from timeout_budgets import (
    CEILING_MARGIN_SECONDS,
    COVERAGE_ACTION,
    NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS,
    TERMINATION_SAFETY_MARGIN_SECONDS,
    NextestConfigurationError,
    UnboundedTestError,
    required_ceiling,
    seconds,
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
            Fraction(180),
            id="a-single-period",
        ),
        pytest.param(
            _profile('slow-timeout = { period = "60s", terminate-after = 5 }'),
            Fraction(300),
            id="five-warning-periods",
        ),
        pytest.param(
            _profile('slow-timeout = { period = "2m", terminate-after = 3 }'),
            Fraction(360),
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
            Fraction(60),
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
            Fraction(45),
            id="a-filter-naming-the-key-is-not-a-budget",
        ),
    ],
)
def test_the_largest_per_test_allowance_counts_the_multiplier(
    config_text: str, expected: Fraction
) -> None:
    """``terminate-after`` scales the period; the budget is their product.

    This is the reading every comparison above rests on, and it is the
    one easy to get wrong. It is driven with controlled configurations
    rather than this repository's own, whose multipliers are all one:
    against that file a reading that ignored the multiplier entirely
    would give the same answer, and the test would prove nothing.
    """
    assert largest_test_allowance(config_text) == expected, (
        f"{config_text!r} must yield a {float(expected):.0f}s largest per-test "
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
    assert largest_test_allowance(config_text) == Fraction(180), (
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
    assert grace_period(config_text) == Fraction(5), (
        "a commented-out grace period was read as the one in force"
    )
    assert (
        termination_allowance(config_text)
        == Fraction(5) + TERMINATION_SAFETY_MARGIN_SECONDS
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
    assert global_timeout(_profile('global-timeout = "10m"')) == Fraction(600)


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
    assert largest_test_allowance(config_text) == Fraction(30), (
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
    assert unset == (
        NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS + TERMINATION_SAFETY_MARGIN_SECONDS
    ), "an unset grace period must fall back to nextest's own default"
    configured = termination_allowance(
        _profile(
            'slow-timeout = { period = "30s", terminate-after = 1, '
            'grace-period = "5s" }'
        )
    )
    assert configured == Fraction(5) + TERMINATION_SAFETY_MARGIN_SECONDS, (
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
    assert largest == Fraction(45) + TERMINATION_SAFETY_MARGIN_SECONDS, (
        "the largest configured grace period governs the allowance"
    )


def test_the_required_ceiling_carries_all_three_terms() -> None:
    """Watchdogs, measured work outside them, and the margin.

    Both ceilings clear the smaller requirement too, so dropping the
    margin changes nothing the assertion over the workflows can see.
    Driving the derivation with controlled numbers is what makes the
    missing term visible.
    """
    assert required_ceiling(
        [Fraction(1800), Fraction(2700)], Fraction(1200)
    ) == Fraction(4500) + Fraction(1200) + CEILING_MARGIN_SECONDS, (
        "two watchdogs, the allowance and the margin are all added"
    )
    assert required_ceiling([Fraction(1800)], Fraction(0)) == (
        Fraction(1800) + CEILING_MARGIN_SECONDS
    ), "the margin applies even when nothing runs outside the watchdog"
    assert required_ceiling([], Fraction(0)) == CEILING_MARGIN_SECONDS, (
        "the margin is a term of its own, not a fraction of the others"
    )


@pytest.mark.parametrize(
    ("duration", "expected"),
    [
        pytest.param("120s", Fraction(120), id="one-component"),
        pytest.param("1m 30s", Fraction(90), id="two-components-spaced"),
        pytest.param("1m30s", Fraction(90), id="two-components-joined"),
        pytest.param("1h 30m 15s", Fraction(5415), id="three-components"),
        pytest.param("1day", Fraction(86400), id="an-extended-unit"),
        pytest.param("1w", Fraction(604800), id="a-week"),
        pytest.param("15min", Fraction(900), id="a-long-unit-spelling"),
        pytest.param("500ms", Fraction(1, 2), id="milliseconds"),
        pytest.param("1.5m", Fraction(90), id="a-fractional-value"),
        pytest.param("1 . 5 m", Fraction(90), id="a-fractional-value-spaced"),
        pytest.param("2wk", Fraction(1209600), id="an-abbreviated-week"),
        pytest.param("1yr", Fraction(31557600), id="an-abbreviated-year"),
        pytest.param("500\u00b5s", Fraction(1, 2000), id="the-micro-sign"),
        pytest.param("1 0s", Fraction(10), id="whitespace-inside-a-number"),
        pytest.param("0", Fraction(0), id="a-bare-zero"),
        pytest.param("0.5s 0.5s", Fraction(1), id="two-halves-carry-to-one-second"),
        pytest.param(
            "18446744073709551615s 999999999ns",
            Fraction(18446744073709551615) + Fraction(999999999, 1_000_000_000),
            id="one-nanosecond-short-of-the-ceiling",
        ),
        pytest.param(
            "18446744073709551615ns 1ns",
            Fraction(18446744073709551616, 1_000_000_000),
            id="nanoseconds-normalized-between-components",
        ),
    ],
)
def test_the_duration_grammar_matches_the_one_nextest_reads(
    duration: str, expected: Fraction
) -> None:
    """nextest deserializes durations with ``humantime``, not one unit.

    A reader accepting a single short-unit component refuses `"1m 30s"`
    and `"1day"`, which nextest loads without complaint, so the contract
    would fail on a configuration that is correct and the failure would
    name the file rather than the reader that could not read it. Every
    spelling here is one ``humantime`` accepts, read from its 2.3.0
    source rather than assumed: a component may carry a fractional part,
    whitespace is skipped wherever a digit could go so ``"1 0s"`` is ten
    seconds, and ``wk``, ``wks``, ``yr``, ``yrs`` and ``\u00b5s`` are
    units alongside the longer spellings.

    ``"0"`` is the one duration written without a unit.
    ``parse_duration`` opens with ``if s == "0"``, compared against the
    untrimmed string, so the bare form is zero and the padded form is
    not; the refusal list carries ``" 0 "`` for that reason.
    """
    assert seconds(duration) == expected, (
        f"{duration!r} must read as {expected} seconds"
    )


@pytest.mark.parametrize(
    "duration",
    [
        pytest.param("120", id="no-unit"),
        pytest.param("s", id="no-value"),
        pytest.param("-30s", id="negative"),
        pytest.param("120 fortnights", id="an-unknown-unit"),
        pytest.param("", id="empty"),
        pytest.param(".5s", id="a-leading-point"),
        pytest.param("1.s", id="a-trailing-point"),
        pytest.param("1.2.3s", id="a-second-point"),
        pytest.param("0.0000000002s", id="finer-than-a-nanosecond"),
        pytest.param("1.5ns", id="a-fraction-of-a-nanosecond"),
        pytest.param("18446744073709551616s", id="past-the-range-humantime-holds"),
        pytest.param("600000000000y", id="a-value-that-overflows-its-unit"),
        pytest.param(
            "18446744073709551615s 500ms 500ms",
            id="a-carry-that-passes-the-ceiling",
        ),
        pytest.param(
            "18446744073709551615ns 18446744073709551615ns",
            id="a-nanosecond-part-that-overflows-before-it-carries",
        ),
        pytest.param(
            "1.00000000000000000000s",
            id="a-denominator-past-the-range-humantime-holds",
        ),
        pytest.param("\u0661s", id="an-arabic-indic-digit"),
        pytest.param(" 0 ", id="a-padded-bare-zero"),
        pytest.param("1\x1cs", id="a-file-separator-inside-a-number"),
        pytest.param("\x1c45m", id="a-file-separator-leading"),
        pytest.param("45m\x1f", id="a-unit-separator-trailing"),
        pytest.param("1\x1d0s", id="a-group-separator-between-digits"),
    ],
)
def test_a_duration_nextest_would_refuse_is_refused_here(duration: str) -> None:
    r"""The grammar is matched, not merely widened.

    ``humantime`` takes values with units and nothing else, so a reader
    accepting more would put a number on a configuration nextest fails
    to load, and the ordering would then be checked against a budget
    nothing enforces. A fractional part is allowed, but a leading point,
    a trailing point and a second point are each refused by
    ``humantime`` and so are refused here.

    So are two limits that a floating-point reading would not have.
    ``humantime`` divides a fraction into its unit and errors on any
    remainder, so a value finer than a nanosecond is refused rather than
    rounded, and a fraction of a nanosecond is refused outright. Every
    intermediate is also held in a ``u64``, so a value past that range
    is refused rather than becoming a large float. The denominator is
    one of those intermediates: ``humantime`` multiplies it by ten per
    fractional digit with a checked multiplication, so twenty fractional
    digits overflow it even when the numerator is zero.

    Digits are ASCII. ``humantime`` matches ``'0'..='9'`` and nothing
    else, so an Arabic-Indic digit is refused there; a reader whose
    pattern used Python's ``\d`` would accept it and put a number on a
    configuration nextest cannot load.

    The carry into seconds is the last of these. Nanoseconds reaching
    exactly one billion are a whole second, so
    ``18446744073709551615s 500ms 500ms`` carries one second past
    ``u64::MAX`` and ``humantime`` refuses it; a reader carrying only
    above a billion returns a duration a second past the ceiling
    instead. Its companions are in the acceptance list: the same
    seconds with ``999999999ns``, one nanosecond short and therefore
    fine, and ``0.5s 0.5s``, which carries to exactly one second and
    shows that the strict-or-equal carry does not over-refuse.

    Where that carry happens is a refusal of its own.
    ``humantime`` normalizes its running total after every whole part
    and every fraction, so a nanosecond part that overflows a ``u64``
    *at one addition* is refused however small the duration it names:
    ``18446744073709551615ns`` twice over is thirty-seven seconds and
    is refused for that reason. Its acceptance companion is in the
    other list, the same value plus ``1ns``, which a reader summing
    into one nanosecond accumulator refuses and ``humantime`` reads.

    ``" 0 "`` is a refusal rather than an acceptance because
    ``parse_duration``'s zero shortcut compares the untrimmed string:
    padded, the shortcut misses and the parser then finds a number with
    no unit, which is `UnknownUnit`.

    The refusal is now a `NextestConfigurationError` rather than an
    `AssertionError`. It is the error the rest of this reading reports
    faults with, so a caller catching `TimeoutBudgetError` gets a
    finding instead of a crash, and `python -O` cannot strip the check.
    """
    with pytest.raises(NextestConfigurationError):
        seconds(duration)


def test_minutes_and_months_are_told_apart() -> None:
    """``m`` is minutes and ``M`` is months, and ``humantime`` is exact.

    Folding case here would read a ten-minute whole-run budget as a
    two-and-a-half-year one, or the reverse, and either reading puts a
    plausible number on the wrong tier.
    """
    assert seconds("10m") == Fraction(600), "m is minutes"
    assert seconds("10M") == Fraction(10 * 2630016), "M is months"


@pytest.mark.parametrize(
    ("name", "contents"),
    [
        pytest.param("broken.yml", b"jobs: [unclosed", id="not-yaml"),
        pytest.param("stray.yml", b"jobs:\n  build:\n    name: \xff\n", id="not-utf-8"),
    ],
)
def test_an_unreadable_workflow_is_reported_as_one(
    tmp_path: pathlib.Path, name: str, contents: bytes
) -> None:
    """The loader promises one error type, so it must catch both faults.

    `yaml.YAMLError` and `UnicodeDecodeError` reach the loader by
    different routes: the second is a `ValueError`, not an `OSError`,
    so a workflow carrying a stray byte would otherwise escape the
    documented contract and surface as a decoding error from what the
    caller reads as a load.
    """
    (tmp_path / name).write_bytes(contents)
    with pytest.raises(WorkflowReadError):
        load_workflow_documents(tmp_path)


@pytest.mark.parametrize(
    ("case", "prepare"),
    [
        pytest.param("absent", lambda root: root / "gone", id="no-directory"),
        pytest.param("empty", lambda root: root, id="no-documents"),
    ],
)
def test_a_directory_with_no_workflows_is_refused_rather_than_read_as_none(
    tmp_path: pathlib.Path,
    case: str,
    prepare: object,
) -> None:
    """Finding nothing is a failure, not an empty answer.

    Every assertion downstream quantifies over the coverage lanes, so a
    loader returning no documents makes all of them vacuous: the
    ordering contract iterates an empty tuple and reports success for a
    repository whose workflows it never opened. A typo in the path, a
    rename of `.github/workflows`, or a checkout without it would all
    have read as "the workflows are fine".

    Both empty shapes are driven, because they arrive by different
    routes: a missing directory never globs, and a present one holding
    nothing that parses globs and keeps nothing.
    """
    root = prepare(tmp_path)  # type: ignore[operator]
    if case == "empty":
        (root / "notes.txt").write_text("not a workflow", encoding="utf-8")
    with pytest.raises(WorkflowReadError):
        load_workflow_documents(root)


@pytest.mark.parametrize(
    ("field", "document"),
    [
        pytest.param(
            "timeout-minutes",
            {
                "jobs": {
                    "build": {
                        "timeout-minutes": "${{ inputs.ceiling }}",
                        "env": {"RUN_RUST_CARGO_WAIT_TIMEOUT": "1800"},
                        "steps": [{"uses": f"{COVERAGE_ACTION}@0000000"}],
                    }
                }
            },
            id="job-ceiling",
        ),
        pytest.param(
            "RUN_RUST_CARGO_WAIT_TIMEOUT",
            {
                "jobs": {
                    "build": {
                        "timeout-minutes": 66,
                        "env": {"RUN_RUST_CARGO_WAIT_TIMEOUT": "30m"},
                        "steps": [{"uses": f"{COVERAGE_ACTION}@0000000"}],
                    }
                }
            },
            id="watchdog",
        ),
    ],
)
def test_a_duration_that_is_not_a_number_is_refused_by_name(
    field: str, document: dict[str, object]
) -> None:
    """The query says what it does with a value it cannot read.

    `coverage_jobs_in` takes documents and does no filesystem work, but
    it still reads declared durations, and both of these reach
    `Fraction` as text. A workflow-expression ceiling and a watchdog
    written with a duration suffix are the two shapes that actually
    occur; either raised a bare `ValueError` naming neither the
    workflow, the job, nor the field, from a function whose docstring
    called it a query.

    The refusal carries all three, and the failure names the field so a
    reader knows which of the two durations was unreadable without
    reconstructing it from a traceback.
    """
    with pytest.raises(WorkflowValueError) as caught:
        coverage_jobs_in({"ci.yml": document})
    assert caught.value.field == field, "the refusal must name the field"
    assert caught.value.location == "ci.yml:build", (
        "the refusal must name the workflow and the job"
    )


def test_budgets_beyond_the_exact_float_range_stay_distinguishable() -> None:
    """Two budgets nextest reads as different must not compare equal here.

    ``read`` returned a float, and above two to the fifty-third a float
    no longer holds every integer second. ``9007199254740993s`` and
    ``9007199254740992s`` are one second apart, both inside the ``u64``
    range ``humantime`` accepts, and the same number once rounded. The
    ordering tier compares the whole-run budget against the largest
    per-test allowance directly, so a configuration where the per-test
    allowance genuinely exceeds the whole run would have compared equal
    and passed a strict ordering it violates.

    The budgets are exact now. The float collapse is asserted alongside,
    because it is the thing being avoided rather than an incidental
    detail, and a reader of this test should not have to take it on
    trust.
    """
    larger = seconds("9007199254740993s")
    smaller = seconds("9007199254740992s")

    assert larger != smaller, "budgets one second apart must not compare equal"
    assert larger > smaller, "the larger budget must order above the smaller"
    assert float(larger) == float(smaller), (
        "the float collapse this guards against must still be real; if these "
        "differ, the case no longer exercises what it was written for"
    )


def test_a_fractional_budget_is_exact_rather_than_rounded() -> None:
    """A budget humantime reads exactly is held exactly, not to a float.

    ``humantime`` refuses a fraction that does not divide into its unit,
    so every duration it accepts has an exact value in seconds. Holding
    that value as a float would reintroduce the rounding the grammar
    went to some trouble to refuse.
    """
    assert seconds("0.1s") == Fraction(1, 10), "a tenth of a second is exact"
    assert seconds("0.1s") * 3 == Fraction(3, 10), (
        "exact budgets stay exact under the arithmetic the tiers apply"
    )


def test_the_terminate_after_product_is_exact() -> None:
    """The per-test budget is a product, and the product must stay exact.

    ``largest_test_allowance`` multiplies the period by
    ``terminate-after``. Done in floating point, a tenth of a second
    three times over is not three tenths, and the tier that compares
    this against the whole-run budget inherits the error. The exactness
    has to survive the multiplication, not only the reading, which is a
    separate place to lose it and was lost there first.
    """
    config_text = _profile('slow-timeout = { period = "0.1s", terminate-after = 3 }')

    assert largest_test_allowance(config_text) == Fraction(3, 10), (
        "a tenth of a second taken three times is three tenths exactly"
    )
    assert float(Fraction(1, 10)) * 3.0 != 0.3, (
        "the floating-point product this guards against must still be wrong; "
        "if it is not, the case no longer exercises what it was written for"
    )


def test_a_table_omitting_the_grace_period_takes_nextest_s_own_default() -> None:
    """The default is per ``slow-timeout`` table, not per file.

    ``grace-period`` is a field of the ``slow-timeout`` setting, and
    nextest fills an omitted field with ten seconds wherever the table
    appears. A reader that skipped tables without the field would report
    this file's five seconds while nextest allows an overridden binary
    ten, and the watchdog floor derived from it would be five seconds
    short.

    Both orderings are driven, because a reader could also take the
    first table it saw rather than the largest.
    """
    omitted_second = _profile(
        'slow-timeout = { period = "30s", terminate-after = 1, grace-period = "5s" }',
        "",
        "[[profile.default.overrides]]",
        'filter = "binary(=ui)"',
        'slow-timeout = { period = "60s", terminate-after = 1 }',
    )
    omitted_first = _profile(
        'slow-timeout = { period = "30s", terminate-after = 1 }',
        "",
        "[[profile.default.overrides]]",
        'filter = "binary(=ui)"',
        'slow-timeout = { period = "60s", terminate-after = 1, '
        'grace-period = "5s" }',
    )
    for config_text in (omitted_second, omitted_first):
        assert grace_period(config_text) == NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS, (
            "a table omitting grace-period allows nextest's ten-second "
            "default, which is longer than the five seconds the other "
            "table names"
        )
    assert NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS > Fraction(5), (
        "this case only discriminates while the default exceeds the "
        "configured five seconds; if it no longer does, the pair no "
        "longer exercises what it was written for"
    )


def test_a_non_string_grace_period_is_refused_rather_than_defaulted() -> None:
    """A present-but-invalid field is not an absent one.

    nextest reads ``grace-period`` through ``humantime_serde``, which
    takes a duration string, so ``grace-period = 10`` makes the whole
    configuration unloadable. Folding that into the ten-second default
    would have this reader report a budget for a file no run can use,
    and the ordering contract would pass on it.

    The two shapes are driven together because only the pair separates
    the states: the omitting table must still take the default, or the
    refusal could have been a reader that stopped reading the field.
    """
    invalid = _profile(
        'slow-timeout = { period = "30s", terminate-after = 1, grace-period = 10 }'
    )
    with pytest.raises(NextestConfigurationError) as caught:
        grace_period(invalid)
    assert caught.value.field == "grace-period", (
        "the refusal must name the field that is wrong"
    )
    assert caught.value.value == 10, (
        "the refusal must carry the value it refused"
    )

    omitted = _profile('slow-timeout = { period = "30s", terminate-after = 1 }')
    assert grace_period(omitted) == NEXTEST_DEFAULT_GRACE_PERIOD_SECONDS, (
        "an omitted grace-period is still nextest's own default; only a "
        "present non-string is a configuration error"
    )


def test_a_neighbouring_action_is_not_read_as_a_coverage_lane() -> None:
    """The coordinate is matched exactly, not by containment.

    A step whose path merely starts with the coverage action's, such as
    a ``generate-coverage-old`` kept beside it through a migration, is a
    different action with different steps. Read as a coverage lane it
    would contribute its own watchdog to the ceiling arithmetic and be
    required to declare one, so the contract would fail on a workflow
    that is correct, or pass on the strength of an unrelated step.

    The genuine coordinate is asserted alongside, so a matcher that
    stopped recognizing coverage lanes altogether fails here rather than
    turning every assertion above into a vacuous pass.
    """
    ref = "@" + "0" * 40
    documents = {
        "ci.yml": {
            "jobs": {
                "neighbour": {
                    "timeout-minutes": 66,
                    "steps": [{"uses": f"{COVERAGE_ACTION}-old{ref}"}],
                },
                "coverage": {
                    "timeout-minutes": 66,
                    "steps": [{"uses": f"{COVERAGE_ACTION}{ref}"}],
                },
            }
        }
    }

    jobs = coverage_jobs_in(documents)

    assert [job.job for job in jobs] == ["coverage"], (
        "only the job using the coverage action itself is a coverage lane"
    )
