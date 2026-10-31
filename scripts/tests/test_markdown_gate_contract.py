"""Hold the local Markdown gate to the CI step it mirrors.

The pure judgements live in ``markdown_gate``; this module supplies the
reference fixtures, drives each judgement against a mutated copy first,
and asserts the real workflow, Makefile and configuration last. A check
run only over the correct files passes whether or not it discriminates,
so the mutations are what make the contract evidence.
"""

from __future__ import annotations

import typing as typ
from pathlib import Path

import pytest
from markdown_gate import (
    ACTION,
    config_faults,
    glob_faults,
    mdlint_faults,
    pairing_faults,
    pairing_faults_for,
    prerequisite_faults,
)
from workflow_reader import Workflow

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MAKEFILE = "Makefile"
CONFIG = ".markdownlint-cli2.jsonc"
CI = ".github/workflows/ci.yml"

#: A reference Makefile with the contract's shape, used for mutations.
_GOOD_MAKEFILE = """\
MARKDOWNLINT_CLI2_VERSION ?= 0.23.2
MDLINT ?= npx --yes markdownlint-cli2@$(MARKDOWNLINT_CLI2_VERSION)

all: check-fmt markdownlint lint

fmt: ## Format sources
\t$(MDLINT) --fix

markdownlint: spelling ## Lint Markdown
\t$(MDLINT)
"""

#: A reference workflow with the contract's shape, used for mutations.
_GOOD_CI = f"""
"on": pull_request
jobs:
  build-test:
    steps:
      - name: Markdown lint
        uses: {ACTION}@21c1be1b93ad9ed58fa840aacc3f279cde2a72ff
        with:
          globs: ''
"""

#: A reference configuration with the contract's shape, used for mutations.
_GOOD_CONFIG = """\
{
  // Selection lives only here.
  "globs": ["**/*.md"],
  "gitignore": true
}
"""


def parse_ci(text: str) -> Workflow:
    """Parse a workflow fragment through the strict loader."""
    return Workflow.parse(CI, text)


def test_the_reference_inputs_satisfy_the_contract() -> None:
    """The reference fixtures are fault-free, so each mutation isolates one."""
    assert glob_faults(parse_ci(_GOOD_CI)) == []
    assert pairing_faults_for(parse_ci(_GOOD_CI), _GOOD_MAKEFILE) == []
    assert prerequisite_faults(_GOOD_MAKEFILE, "all", ["markdownlint"]) == []
    assert mdlint_faults(_GOOD_MAKEFILE, "markdownlint") == []
    assert mdlint_faults(_GOOD_MAKEFILE, "fmt") == []
    assert config_faults(_GOOD_CONFIG) == []


@pytest.mark.parametrize(
    ("old", "new"),
    [
        ("          globs: ''\n", ""),
        ("globs: ''", "globs: '**/*.md'"),
    ],
    ids=["missing", "caller-glob"],
)
def test_globs_passed_to_the_action_are_reported(old: str, new: str) -> None:
    """Only an explicit empty input leaves selection to the configuration."""
    assert old in _GOOD_CI, f"the mutation target {old!r} must exist"
    assert glob_faults(parse_ci(_GOOD_CI.replace(old, new)))


@pytest.mark.parametrize(
    ("revision", "version"),
    [
        ("21c1be1b93ad9ed58fa840aacc3f279cde2a72ff", "0.23.1"),
        ("deadbeef", "0.23.2"),
        ("", "0.23.2"),
    ],
    ids=["version-mismatch", "unapproved-revision", "no-revision"],
)
def test_an_unapproved_or_mismatched_pairing_is_refused(
    revision: str, version: str
) -> None:
    """An off-list revision or a mismatched CLI version is reported."""
    assert pairing_faults(revision, version)


def test_a_missing_all_prerequisite_is_reported() -> None:
    """Dropping `markdownlint` from `all` reopens the hole in the local gate."""
    shortened = _GOOD_MAKEFILE.replace(
        "all: check-fmt markdownlint lint", "all: check-fmt lint"
    )
    assert shortened != _GOOD_MAKEFILE, "the reference `all` line must exist"
    assert prerequisite_faults(shortened, "all", ["markdownlint"])


@pytest.mark.parametrize(
    ("target", "old", "new"),
    [
        ("markdownlint", "$(MDLINT)\n", '$(MDLINT) "**/*.md"\n'),
        ("markdownlint", "$(MDLINT)\n", '$(MDLINT) "#.uv-cache"\n'),
        ("markdownlint", "$(MDLINT)\n", "@echo $(MDLINT)\n"),
        ("fmt", "$(MDLINT) --fix\n", '$(MDLINT) --fix "**/*.md"\n'),
        ("fmt", "$(MDLINT) --fix\n", "$(MDLINT) --fix #.uv-cache\n"),
    ],
    ids=[
        "lint-glob",
        "lint-exclusion",
        "lint-echo-only",
        "fmt-glob",
        "fmt-exclusion",
    ],
)
def test_caller_globs_in_a_recipe_are_reported(
    target: str, old: str, new: str
) -> None:
    """A recipe that names files or exclusions splits the selection in two."""
    assert old in _GOOD_MAKEFILE, f"the mutation target {old!r} must exist"
    assert mdlint_faults(_GOOD_MAKEFILE.replace(old, new), target)


@pytest.mark.parametrize(
    ("old", "new"),
    [
        ('"globs": ["**/*.md"],', '"globs": [],'),
        ('"globs": ["**/*.md"],', '"globs": null,'),
        ('"globs": ["**/*.md"],', ""),
        ('"globs": ["**/*.md"],', '"globs": ["docs/**/*.md"],'),
        (
            '"globs": ["**/*.md"],',
            '"globs": ["**/*.md", "!**/*.md"],',
        ),
    ],
    ids=[
        "empty",
        "not-an-array",
        "missing-key",
        "partial-selection",
        "cancelling-negation",
    ],
)
def test_empty_or_missing_configuration_globs_are_reported(
    old: str, new: str
) -> None:
    """Selection must be a non-empty array covering every Markdown file."""
    assert old in _GOOD_CONFIG, f"the mutation target {old!r} must exist"
    assert config_faults(_GOOD_CONFIG.replace(old, new))


def test_a_negation_of_a_non_markdown_pattern_is_allowed() -> None:
    """Negating a pattern that cannot remove Markdown keeps the selection whole."""
    config = _GOOD_CONFIG.replace(
        '"globs": ["**/*.md"],', '"globs": ["**/*.md", "!**/*.json"],'
    )
    assert config != _GOOD_CONFIG, "the reference globs line must exist"
    assert config_faults(config) == []


def test_the_repository_holds_the_contract() -> None:
    """The real workflow, Makefile and configuration satisfy every judgement."""
    workflow = parse_ci((REPOSITORY_ROOT / CI).read_text(encoding="utf-8"))
    makefile = (REPOSITORY_ROOT / MAKEFILE).read_text(encoding="utf-8")
    configuration = (REPOSITORY_ROOT / CONFIG).read_text(encoding="utf-8")
    faults: dict[str, list[str]] = {
        "action globs": glob_faults(workflow),
        "action/CLI pairing": pairing_faults_for(workflow, makefile),
        "make all membership": prerequisite_faults(
            makefile, "all", ["markdownlint"]
        ),
        "markdownlint recipe": mdlint_faults(makefile, "markdownlint"),
        "fmt recipe": mdlint_faults(makefile, "fmt"),
        "configuration globs": config_faults(configuration),
    }
    assert not any(faults.values()), faults