"""Static contracts for the Loom inventory, the builder and the Makefile.

The runner trusts `.config/loom-models.toml` to name every model, and the
models trust `src/loom_model.rs` to be the only place that builds a Loom
builder. These tests read the source and the Makefile directly, so a new
ignored model, a second builder, or a recipe that falls back to a bare
`-- --ignored` fails without compiling anything.
"""

from __future__ import annotations

import re
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
MANIFEST = REPO_ROOT / ".config" / "loom-models.toml"
MAKEFILE = REPO_ROOT / "Makefile"
LOOM_MODEL = REPO_ROOT / "src" / "loom_model.rs"

IGNORE = '#[ignore = "requires Loom model checking"]'
FUNCTION = re.compile(r"^fn\s+([a-z0-9_]+)\s*\(", re.MULTILINE)


def module_path(source: Path) -> str:
    """Return the Rust module path a Loom test file declares.

    Examples
    --------
    >>> module_path(Path("src/env/loom_tests/mod.rs"))
    'env::loom_tests'
    >>> module_path(Path("src/cluster/lifecycle_loom_tests.rs"))
    'cluster::lifecycle_loom_tests'
    """
    relative = source.relative_to(REPO_ROOT / "src")
    parts = list(relative.with_suffix("").parts)
    if parts[-1] == "mod":
        parts.pop()
    return "::".join(parts)


def is_loom_source(path: Path) -> bool:
    """Return whether `path` is a Loom test module or lives in one.

    Examples
    --------
    >>> is_loom_source(Path("src/env/loom_tests/mod.rs"))
    True
    >>> is_loom_source(Path("src/cluster/lifecycle_loom_tests.rs"))
    True
    >>> is_loom_source(Path("src/env/mod.rs"))
    False
    """
    names = (path.stem, path.parent.name)
    return any(name == "loom_tests" or name.endswith("_loom_tests") for name in names)


def loom_sources() -> list[Path]:
    """Return the source files that hold Loom models."""
    return sorted(
        path for path in (REPO_ROOT / "src").rglob("*.rs") if is_loom_source(path)
    )


def ignored_models(source: Path) -> list[str]:
    """Return the full names of the ignored models in `source`."""
    text = source.read_text(encoding="utf-8")
    names: list[str] = []
    for match in re.finditer(re.escape(IGNORE), text):
        function = FUNCTION.search(text, match.end())
        if function is not None:
            names.append(f"{module_path(source)}::{function.group(1)}")
    return names


def manifest_names() -> list[str]:
    """Return the model names the manifest declares."""
    document = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    return [model["name"] for model in document["models"]]


def test_the_manifest_names_every_ignored_loom_model() -> None:
    """A new ignored model must be registered, and a stale entry removed."""
    declared = set(manifest_names())
    found = {name for source in loom_sources() for name in ignored_models(source)}

    assert found, "no ignored Loom models were found in the source"
    assert declared == found, (
        "the manifest and the ignored Loom models must name the same models"
    )


def test_the_manifest_declares_both_profiles() -> None:
    """Both profiles carry a positive per-model timeout and whole-run budget."""
    document = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))

    for profile in ("pr", "scheduled"):
        assert document["profiles"][profile]["model_timeout_seconds"] > 0, (
            f"the {profile} profile needs a positive model timeout"
        )
        assert document["profiles"][profile]["run_budget_seconds"] > 0, (
            f"the {profile} profile needs a positive run budget"
        )


def test_the_shared_runner_is_the_only_builder_site() -> None:
    """Every model goes through `src/loom_model.rs`, so the bounds stay shared."""
    sites = [
        path
        for path in (REPO_ROOT / "src").rglob("*.rs")
        if "loom::model::Builder::new()" in path.read_text(encoding="utf-8")
    ]

    assert sites == [LOOM_MODEL], (
        "every Loom builder must come from the shared model runner"
    )


def loom_recipe() -> list[str]:
    """Return the recipe lines of the `test-loom` target."""
    lines = MAKEFILE.read_text(encoding="utf-8").splitlines()
    start = next(
        index for index, line in enumerate(lines) if line.startswith("test-loom:")
    )
    recipe: list[str] = []
    for line in lines[start + 1 :]:
        if not line.startswith("\t"):
            break
        recipe.append(line)
    return recipe


def test_the_makefile_runs_the_runner_without_a_bare_ignored_filter() -> None:
    """`make test-loom` selects models through the manifest, not `-- --ignored`."""
    recipe = "\n".join(loom_recipe())

    assert "run --script scripts/loom_runner.py" in recipe, (
        "the recipe must go through the runner"
    )
    assert "LOOM_PROFILE" in recipe, "the recipe must forward the profile"
    assert "LOOM_OUTPUT_DIR" in recipe, "the recipe must forward the report directory"
    assert "-- --ignored" not in recipe, (
        "the recipe must not fall back to a bare ignored filter"
    )


def test_the_makefile_preserves_the_composed_rustflags() -> None:
    """The runner inherits the standard flags the other recipes compose."""
    recipe = loom_recipe()

    assert recipe[0].startswith('\tRUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }'), (
        "the recipe must compose the caller's RUSTFLAGS"
    )
    assert "$(STANDARD_RUSTFLAGS)" in recipe[0], (
        "the recipe must apply the standard Rust flags"
    )