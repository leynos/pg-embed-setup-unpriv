"""Parse and validate the Loom model inventory.

`.config/loom-models.toml` is the single list of models the runner must
execute. This module turns that document into `Manifest` and `Model` values,
rejects malformed or incomplete entries, and checks a discovered inventory
against it. Doing that here keeps the discovery and execution modules small
enough to read in one sitting.

Examples
--------
>>> parse_manifest(
...     "[profiles.pr]\\nmodel_timeout_seconds = 10\\nrun_budget_seconds = 100\\n"
...     "[profiles.scheduled]\\nmodel_timeout_seconds = 20\\nrun_budget_seconds = 200\\n"
...     '[[models]]\\nname = "env::loom_tests::one"\\nsuite = "env"\\n'
...     "spawned_threads = 0\\ncontext_threads = 1\\n"
... ).timeouts["pr"]
10
"""

from __future__ import annotations

import tomllib
import typing as typ
from dataclasses import dataclass
from pathlib import Path

#: The profiles the manifest must describe.
PROFILES: typ.Final = ("pr", "scheduled")


class DiscoveryError(Exception):
    """Raised when the inventory or the compiled test binary cannot be trusted."""


@dataclass(frozen=True)
class Model:
    """One expected Loom model.

    Attributes
    ----------
    name : str
        The full libtest name, including its module path.
    suite : str
        The module path prefix that groups the model.
    spawned_threads : int
        The threads the model itself spawns.
    context_threads : int
        The total participants, including the libtest execution thread.
    """

    name: str
    suite: str
    spawned_threads: int
    context_threads: int


@dataclass(frozen=True)
class Manifest:
    """The validated inventory and its per-profile wall-clock limits.

    Attributes
    ----------
    models : tuple[Model, ...]
        The expected models, in execution order.
    timeouts : dict[str, int]
        Profile name to per-model timeout in seconds.
    budgets : dict[str, int]
        Profile name to the whole-run budget in seconds, covering the build,
        discovery and every model, so the run still writes its summary when
        several models degrade at once.
    """

    models: tuple[Model, ...]
    timeouts: dict[str, int]
    budgets: dict[str, int]


def _positive_int(value: object, description: str) -> int:
    """Return `value` as a positive integer, or raise `DiscoveryError`.

    Examples
    --------
    >>> _positive_int(3, "count")
    3
    >>> _positive_int(0, "count")
    Traceback (most recent call last):
    ...
    loom_manifest.DiscoveryError: count must be a positive integer
    """
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise DiscoveryError(f"{description} must be a positive integer")
    return value


def _non_negative_int(value: object, description: str) -> int:
    """Return `value` as a non-negative integer, or raise `DiscoveryError`.

    Examples
    --------
    >>> _non_negative_int(0, "count")
    0
    >>> _non_negative_int(-1, "count")
    Traceback (most recent call last):
    ...
    loom_manifest.DiscoveryError: count must be a non-negative integer
    """
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise DiscoveryError(f"{description} must be a non-negative integer")
    return value


def _model(entry: object, index: int, source: str) -> Model:
    """Return one manifest entry as a `Model`.

    Raises
    ------
    DiscoveryError
        If the entry is not a table or a field is missing or malformed.
    """
    if not isinstance(entry, dict):
        raise DiscoveryError(f"{source} models[{index}] is not a table")
    name = entry.get("name")
    suite = entry.get("suite")
    if not isinstance(name, str) or not name:
        raise DiscoveryError(f"{source} models[{index}] needs a non-empty name")
    if not isinstance(suite, str) or not suite:
        raise DiscoveryError(f"{source} models[{index}] needs a non-empty suite")
    spawned = _non_negative_int(
        entry.get("spawned_threads"), f"{source} {name} spawned_threads"
    )
    context = _non_negative_int(
        entry.get("context_threads"), f"{source} {name} context_threads"
    )
    if context < spawned + 1:
        raise DiscoveryError(
            f"{source} {name} context_threads must include the execution thread"
        )
    return Model(name, suite, spawned, context)


def parse_manifest(text: str, source: str = "loom-models.toml") -> Manifest:
    """Return the validated inventory declared by `text`.

    Parameters
    ----------
    text : str
        The TOML document.
    source : str
        The name used in error messages.

    Returns
    -------
    Manifest
        The models and the per-profile timeouts.

    Raises
    ------
    DiscoveryError
        If the document is not TOML, declares no models, repeats a model, or
        omits a profile timeout or run budget.

    Examples
    --------
    >>> document = '''
    ... [profiles.pr]
    ... model_timeout_seconds = 10
    ... run_budget_seconds = 100
    ... [profiles.scheduled]
    ... model_timeout_seconds = 20
    ... run_budget_seconds = 200
    ... [[models]]
    ... name = "env::loom_tests::one"
    ... suite = "env"
    ... spawned_threads = 2
    ... context_threads = 3
    ... '''
    >>> manifest = parse_manifest(document)
    >>> manifest.timeouts
    {'pr': 10, 'scheduled': 20}
    >>> manifest.budgets
    {'pr': 100, 'scheduled': 200}
    >>> manifest.models[0].name
    'env::loom_tests::one'
    """
    try:
        document = tomllib.loads(text)
    except tomllib.TOMLDecodeError as err:
        raise DiscoveryError(f"{source} is not valid TOML: {err}") from err
    profiles = document.get("profiles")
    if not isinstance(profiles, dict):
        raise DiscoveryError(f"{source} declares no [profiles] table")
    timeouts: dict[str, int] = {}
    budgets: dict[str, int] = {}
    for profile in PROFILES:
        table = profiles.get(profile)
        if not isinstance(table, dict):
            raise DiscoveryError(f"{source} declares no [profiles.{profile}] table")
        timeouts[profile] = _positive_int(
            table.get("model_timeout_seconds"),
            f"{source} [profiles.{profile}] model_timeout_seconds",
        )
        budgets[profile] = _positive_int(
            table.get("run_budget_seconds"),
            f"{source} [profiles.{profile}] run_budget_seconds",
        )
    entries = document.get("models")
    if not isinstance(entries, list) or not entries:
        raise DiscoveryError(f"{source} declares no [[models]] entries")
    models = tuple(_model(entry, index, source) for index, entry in enumerate(entries))
    names = [model.name for model in models]
    if len(set(names)) != len(names):
        raise DiscoveryError(f"{source} declares a model more than once")
    return Manifest(models, timeouts, budgets)


def load_manifest(path: Path) -> Manifest:
    """Read and validate the manifest at `path`.

    Raises
    ------
    DiscoveryError
        If the file cannot be read or does not validate.
    """
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as err:
        raise DiscoveryError(f"cannot read {path}: {err}") from err
    return parse_manifest(text, str(path))


def is_loom_path(name: str) -> bool:
    """Return whether a libtest name sits under a Loom test module.

    A module is a Loom module when one of its path segments is `loom_tests` or
    ends in `_loom_tests`, which covers `env::loom_tests`,
    `cluster::lifecycle_loom_tests` and `test_support::shared_singleton_loom_tests`.

    Examples
    --------
    >>> is_loom_path("env::loom_tests::one")
    True
    >>> is_loom_path("cluster::lifecycle_loom_tests::one")
    True
    >>> is_loom_path("env::tests::one")
    False
    """
    return any(
        segment == "loom_tests" or segment.endswith("_loom_tests")
        for segment in name.split("::")
    )


def validate_inventory(
    manifest: Manifest, listed: typ.Sequence[str], profile: str
) -> None:
    """Refuse a run whose inventory and compiled binary disagree.

    Parameters
    ----------
    manifest : Manifest
        The expected inventory.
    listed : Sequence[str]
        The names the compiled test binary reports as ignored.
    profile : str
        The selected exploration profile.

    Raises
    ------
    DiscoveryError
        If the profile is unknown, the inventory is empty, an expected model
        is missing, or an unregistered Loom-looking ignored test is present.

    Examples
    --------
    >>> manifest = parse_manifest('''
    ... [profiles.pr]
    ... model_timeout_seconds = 10
    ... run_budget_seconds = 100
    ... [profiles.scheduled]
    ... model_timeout_seconds = 20
    ... run_budget_seconds = 200
    ... [[models]]
    ... name = "env::loom_tests::one"
    ... suite = "env"
    ... spawned_threads = 0
    ... context_threads = 1
    ... ''')
    >>> validate_inventory(manifest, ["env::loom_tests::one"], "pr")
    >>> validate_inventory(manifest, [], "pr")
    Traceback (most recent call last):
    ...
    loom_manifest.DiscoveryError: expected Loom models are not discoverable: env::loom_tests::one
    """
    if profile not in manifest.timeouts:
        raise DiscoveryError(f"unknown Loom profile {profile!r}")
    if not manifest.models:
        raise DiscoveryError("the Loom inventory is empty")
    expected = [model.name for model in manifest.models]
    listed_set = set(listed)
    missing = [name for name in expected if name not in listed_set]
    if missing:
        raise DiscoveryError(
            "expected Loom models are not discoverable: " + ", ".join(missing)
        )
    registered = set(expected)
    unregistered = sorted(
        name for name in listed if is_loom_path(name) and name not in registered
    )
    if unregistered:
        raise DiscoveryError(
            "ignored Loom models are not registered in the manifest: "
            + ", ".join(unregistered)
        )
