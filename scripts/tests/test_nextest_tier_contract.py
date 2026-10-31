"""Contract for the budgets and test groups in ``.config/nextest.toml``.

nextest's default profile bounds every test at 180 s, and three
settings can raise or lower that bound: the profile's own table, an
override matched by a filter, and a test group's slot. This contract
pins the two above-default tiers, the exact binaries each names, and
the one-slot group every cross-process lock consumer joins.

Each budget here is written once and derived from nothing, because an
ordering check alone accepts a wide range of values: a cluster tier
reverted to nextest's 180 s default would still sit below the 900 s
``ui`` override and the 1,100 s whole-run budget, so only a pin by
value catches it. The scope is pinned structurally through the readers
exercised in ``test_nextest_filters_contract.py``, and the order
against the outer tiers is checked in
``test_timeout_ordering_contract.py``.

See "Test timeouts: four tiers, outermost last" in
``docs/developers-guide.md``.
"""

import tomllib
import typing as typ
from fractions import Fraction

from nextest_budgets import bounds_a_single_test, global_timeout
from nextest_filters import (
    EMBEDDED_SERIAL_TERM,
    binary_names_in_filter,
    serial_group_binaries,
)
from timeout_budgets import (
    REQUIRED_GLOBAL_TIMEOUT_SECONDS,
    seconds,
)


#: The default per-test budget, written once here and derived from nothing,
#: so the configuration cannot drift away from the value the guide records.
DEFAULT_BUDGET_SECONDS: typ.Final[Fraction] = Fraction(180)

#: The budget the `binary(=ui)` override gives its tests, written once here
#: and derived from nothing, so the guide and the configuration stay one
#: statement. The slowest macOS trybuild tests measured 316 s.
UI_BUDGET_SECONDS: typ.Final[Fraction] = Fraction(900)

#: The budget the cluster tier gives its tests. Sized against the slowest
#: uncontended cluster test measured on the host #247 was filed from:
#: `test_cluster_fixture unit_tests::fixture_exposes_connection_metadata`
#: took 132.893 s with nothing else in the same nextest run. 300 s is
#: 2.25 times that measurement, leaving 167 s of headroom for a loaded
#: developer host or a cold CI runner.
CLUSTER_BUDGET_SECONDS: typ.Final[Fraction] = Fraction(300)

#: The integration binaries that start or share a real PostgreSQL cluster.
#: Pinned exactly, so a filter widened to `all()` or a neighbouring binary
#: named by accident is a visible change rather than a silent one.
CLUSTER_BINARIES: typ.Final[frozenset[str]] = frozenset(
    {
        "bootstrap_privileges",
        "cluster_split_constructors",
        "data_dir_recovery",
        "database_lifecycle",
        "observability",
        "password_reuse_e2e",
        "settings_logging",
        "shared_cluster_handle_success",
        "shutdown_hook",
        "shutdown_hook_lifecycle",
        "test_cluster_async",
        "test_cluster_behaviour",
        "test_cluster_connection",
        "test_cluster_drop",
        "test_cluster_extensions",
        "test_cluster_fixture",
        "timezone_regression",
    }
)

#: The whole binaries that take the cross-process scenario lock directly.
#: `test_cluster_async` and `data_dir_recovery` are deliberately absent:
#: their scenarios use the local guard and isolated roots, and only their
#: embedded `serial::tests::*` cases take the lock. `shared_cluster_handle_success`
#: also stays out, because it takes no scenario lock at all.
SERIAL_BINARIES: typ.Final[frozenset[str]] = frozenset(
    {
        "bootstrap_privileges",
        "cluster_split_constructors",
        "database_lifecycle",
        "observability",
        "password_reuse_e2e",
        "settings_logging",
        "shutdown_hook",
        "shutdown_hook_lifecycle",
        "test_cluster_behaviour",
        "test_cluster_connection",
        "test_cluster_drop",
        "test_cluster_extensions",
        "test_cluster_fixture",
        "timezone_regression",
    }
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
    assert whole_run == REQUIRED_GLOBAL_TIMEOUT_SECONDS, (
        f"the global-timeout is {whole_run:.0f}s, not the "
        f"{REQUIRED_GLOBAL_TIMEOUT_SECONDS:.0f}s the developers' guide "
        f"states; change the guide with it or change it back"
    )


def test_the_default_profile_bounds_a_test_no_override_matches(
    nextest_config: str,
) -> None:
    """An override bounds its filter's tests; the profile bounds the rest.

    ``largest_test_allowance`` reports the largest budget anywhere in
    the file, so deleting ``[profile.default]``'s own ``slow-timeout``
    and leaving the `ui` and `settings` overrides behind still reports
    900 s while every test those overrides do not match runs with no
    bound at all. Nothing else here would notice.

    Proved by mutation: commenting out the profile's own
    ``slow-timeout`` fails this test and nothing else.
    """
    assert bounds_a_single_test(nextest_config), (
        "[profile.default] itself must set slow-timeout with terminate-after; "
        "an override satisfies the file as a whole while leaving every test it "
        "does not match unbounded"
    )


def test_only_ui_and_the_cluster_tier_exceed_the_default_budget(
    nextest_config: str,
) -> None:
    """Exactly two tiers sit above the default, in the order the guide lists.

    The `ui` tests are trybuild builds that run for about five minutes on a
    macOS runner, so their budget is the largest in the file. The cluster
    tier is the other deliberate rise. Widening either override (to
    ``all()``, or by adding a binary) would quietly give every other test
    the same allowance, and a real hang in them would then take that long
    to fail. A duplicate declaration would let a later one win without
    changing the list this contract checks.

    Proved by mutation: changing either override's filter to ``all()``,
    adding a third above-default override, removing one, swapping their
    order, or duplicating a tier each fails this test.
    """
    profile = tomllib.loads(nextest_config)["profile"]["default"]
    default_budget = _budget(profile["slow-timeout"])
    longer = [
        override
        for override in profile.get("overrides", [])
        if "slow-timeout" in override
        and _budget(override["slow-timeout"]) > default_budget
    ]
    assert len(longer) == 2, (
        f"exactly two overrides may exceed the default {default_budget} s "
        f"per-test budget, but these do: "
        f"{[override['filter'] for override in longer]}"
    )
    assert longer[0]["filter"] == "binary(=ui)", (
        f"`binary(=ui)` must be the first above-default override; found "
        f"{longer[0]['filter']!r}"
    )
    assert _budget(longer[0]["slow-timeout"]) == UI_BUDGET_SECONDS
    assert binary_names_in_filter(str(longer[1]["filter"])) == CLUSTER_BINARIES, (
        "the second above-default override must name every cluster binary and "
        f"nothing else; found {binary_names_in_filter(str(longer[1]['filter']))}"
    )
    assert _budget(longer[1]["slow-timeout"]) == CLUSTER_BUDGET_SECONDS


def _cluster_declaration(nextest_config: str) -> dict[str, object]:
    """Return the single override whose filter names the cluster binaries."""
    # Exactly one declaration: a second would let a later one win without
    # changing the list this contract checks.
    profile = tomllib.loads(nextest_config)["profile"]["default"]
    declarations = [
        override
        for override in profile.get("overrides", [])
        if "slow-timeout" in override
        and binary_names_in_filter(str(override["filter"])) == CLUSTER_BINARIES
    ]
    assert len(declarations) == 1, (
        f"exactly one override must name the {len(CLUSTER_BINARIES)} cluster "
        f"binaries; found {len(declarations)}"
    )
    return declarations[0]


def test_the_cluster_budget_is_the_one_the_measurement_supports(
    nextest_config: str,
) -> None:
    """The cluster tier is pinned by value, scope, multiplier and grace.

    The default 180 s was nextest's own default rather than a figure
    chosen for these tests, and the slowest uncontended cluster test needs
    132.893 s. A budget reverted to 180 s, widened by ``all()`` or by
    adding `ui` or `settings`, given a different multiplier, or left
    without an explicit 5 s grace period each fails here. The grace period
    matters on its own: omitting it selects nextest's 10 s default and
    silently raises the watchdog floor from 1,765 s to 1,770 s.

    Proved by mutation: changing the period, adding a binary to the
    filter, changing ``terminate-after``, or deleting ``grace-period``
    fails this test.
    """
    declaration = _cluster_declaration(nextest_config)
    slow_timeout = declaration["slow-timeout"]

    assert slow_timeout.get("terminate-after") == 1, (
        "the cluster tier must terminate after one period, like every other "
        "table in this file"
    )
    assert _budget(slow_timeout) == CLUSTER_BUDGET_SECONDS, (
        f"the cluster tier must give {CLUSTER_BUDGET_SECONDS:.0f}s, not "
        f"{_budget(slow_timeout):.0f}s"
    )
    grace = slow_timeout.get("grace-period")
    assert isinstance(grace, str), (
        "the cluster tier must state its grace period as a duration string; "
        "omitting it would select nextest's 10 s default and raise the "
        "watchdog floor"
    )
    assert seconds(grace) == Fraction(5), (
        f"the cluster tier must state a 5 s grace period, not {grace!r}"
    )


def test_the_cluster_budget_sits_between_the_default_and_ui(
    nextest_config: str,
) -> None:
    """The new tier is inside both its neighbours, and inside the whole run.

    A cluster budget below the default would make the tier pointless,
    above `ui` would invert the documented order, and above the
    whole-run budget would let the run end before a cluster test could
    use its allowance.
    """
    profile = tomllib.loads(nextest_config)["profile"]["default"]
    default_budget = _budget(profile["slow-timeout"])
    cluster = _budget(_cluster_declaration(nextest_config)["slow-timeout"])
    whole_run = global_timeout(nextest_config)

    assert default_budget == DEFAULT_BUDGET_SECONDS, (
        f"the default per-test budget is {default_budget:.0f}s, not the "
        f"{DEFAULT_BUDGET_SECONDS:.0f}s the guide states"
    )
    assert default_budget < cluster < UI_BUDGET_SECONDS, (
        f"the order must be default < cluster < ui; found "
        f"{default_budget:.0f}s, {cluster:.0f}s, {UI_BUDGET_SECONDS:.0f}s"
    )
    assert whole_run is not None, "no global-timeout is set"
    assert cluster < whole_run, (
        f"the {cluster:.0f}s cluster budget is not inside the "
        f"{whole_run:.0f}s whole-run budget"
    )


def test_the_serial_group_holds_every_lock_consumer(nextest_config: str) -> None:
    """Every cross-process lock user joins the one-slot group, by structure.

    nextest does not count a test's wait for a group slot against
    ``slow-timeout``, but it does count time blocked in ``flock`` after
    the test starts. Moving the direct consumers into the group turns
    their lock wait into queue time; the embedded ``serial::tests::*``
    term covers the same lock's unit tests in every integration binary.

    The exact registry is pinned, and the embedded term must appear once
    and unmodified. A missing serial binary, a missing embedded term, a
    repeated term, ``max-threads`` other than one, or a second override
    putting tests in the group each fails here.

    Proved by mutation: removing a binary from the filter, deleting the
    embedded term, duplicating it, setting ``max-threads = 2``, or adding
    a second ``test-group = "serial"`` override fails this test.
    """
    config = tomllib.loads(nextest_config)
    groups = config.get("test-groups", {})
    assert groups.get("serial") == {"max-threads": 1}, (
        f"the serial group must set max-threads = 1; found "
        f"{groups.get('serial')!r}"
    )

    profile = config["profile"]["default"]
    declarations = [
        override
        for override in profile.get("overrides", [])
        if override.get("test-group") == "serial"
    ]
    assert len(declarations) == 1, (
        f"exactly one override must assign the serial group; found "
        f"{len(declarations)}"
    )
    filter_text = str(declarations[0]["filter"])

    assert serial_group_binaries(filter_text) == SERIAL_BINARIES, (
        f"the serial group must name exactly the "
        f"{len(SERIAL_BINARIES)} direct consumers; found "
        f"{serial_group_binaries(filter_text)}"
    )
    assert EMBEDDED_SERIAL_TERM in filter_text, (
        "the serial group must carry the embedded lock-test term so the "
        "lock's own unit tests do not take it outside the group"
    )


def test_the_ui_binary_budget_is_the_one_the_guide_states(
    nextest_config: str,
) -> None:
    """The `ui` override's budget is pinned by value, not only scoped.

    The scope test above passes for any budget above the default, so
    reverting the override to its old 360 s would leave it green while the
    macOS runner's slow trybuild tests (measured up to 316 s) timed out
    again. The ordering checks accept 360 s as well, because they only
    compare the budget with the whole-run timeout.

    Proved by mutation: setting the override's period back to ``360s`` fails
    this test and nothing else.
    """
    overrides = tomllib.loads(nextest_config)["profile"]["default"]["overrides"]
    budgets = [
        _budget(override["slow-timeout"])
        for override in overrides
        if override.get("filter") == "binary(=ui)" and "slow-timeout" in override
    ]
    assert budgets == [UI_BUDGET_SECONDS], (
        f"the `binary(=ui)` override must give a budget of "
        f"{UI_BUDGET_SECONDS:.0f}s (period times terminate-after); found "
        f"{[f'{budget:.0f}s' for budget in budgets]}"
    )


def _budget(slow_timeout: dict[str, object]) -> Fraction:
    """Return a ``slow-timeout`` table's budget: its period times terminate-after."""
    # A table without terminate-after never terminates the test, so it has
    # no budget to compare; counting it as one period would accept an
    # unbounded timeout as a valid value. The period goes through the
    # shared duration reader, so any spelling nextest accepts reads the
    # same.
    if "terminate-after" not in slow_timeout:
        raise ValueError(
            "a slow-timeout without terminate-after never terminates the test, "
            "so it has no budget"
        )
    return seconds(str(slow_timeout["period"])) * int(
        typ.cast("int", slow_timeout["terminate-after"])
    )
