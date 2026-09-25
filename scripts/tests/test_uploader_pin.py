"""Hold this repository's workflows to the approved CodeScene uploader.

The first tests apply `uploader_pin` to the real workflows. The probes
start from the constructed repository the coverage-shape probes use and
make the one edit a later change could, asserting the rule meant to
catch it fires, and that the readings it depends on are not fooled by a
comment or by the case of the owner.
"""

from __future__ import annotations

import typing as typ
from pathlib import Path

import pytest
from test_coverage_shape_probes import PUBLISHER, repository
from uploader_pin import (
    APPROVED_PIN,
    DEPRECATED,
    NO_UPLOADER,
    deprecated_mentions,
    pin_faults,
    refresh_workflows,
)
from workflow_reader import Workflow, load_workflows

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]

#: The uploader reference in the constructed publisher, and its approved form.
BASE_USES: typ.Final = "upload-codescene-coverage@abc\n"
APPROVED_USES: typ.Final = f"upload-codescene-coverage@{APPROVED_PIN}\n"

#: The constructed publisher with the uploader at the approved pin.
PINNED: typ.Final = PUBLISHER.replace(BASE_USES, APPROVED_USES)

#: A second job calling the uploader, with the owner in another case.
SECOND_JOB: typ.Final = (
    "  second:\n    runs-on: ubuntu-latest\n    steps:\n"
    "      - uses: Leynos/Shared-Actions/.github/actions/"
    "upload-codescene-coverage@main\n"
)

#: The upload step's last input, where a probe adds another.
LAST_INPUT: typ.Final = "          access-token: ${{ secrets.CS_ACCESS_TOKEN }}\n"


@pytest.fixture(scope="module")
def workflows() -> list[Workflow]:
    """Return the repository's parsed workflows once for the module."""
    return load_workflows(REPOSITORY_ROOT)


def test_every_uploader_step_is_pinned_to_the_approved_sha(
    workflows: list[Workflow],
) -> None:
    """One approved SHA, as an allowlist; an empty reading is refused."""
    found = pin_faults(workflows)
    assert found == [], found


@pytest.mark.parametrize("name", DEPRECATED, ids=["input", "variable"])
def test_no_workflow_mentions_a_deprecated_name(
    workflows: list[Workflow], name: str
) -> None:
    """The uploader rejects the input, and the variable only fed it."""
    found = deprecated_mentions(workflows, name)
    assert found == [], f"{name} is still mentioned in {found}"


def test_the_checksum_refresh_workflow_is_absent(workflows: list[Workflow]) -> None:
    """The dispatch that wrote the variable has nothing left to feed."""
    found = refresh_workflows(workflows)
    assert found == [], f"delete {found}; nothing reads the variable it wrote"


def test_the_pinned_base_is_clean() -> None:
    """The constructed base passes, so each probe's failure is its own."""
    assert BASE_USES in PUBLISHER, "the probe's anchor is not in the base"
    found = pin_faults(repository(publisher=PINNED))
    assert found == [], found


def test_an_unapproved_pin_is_named() -> None:
    """Any other ref fails the allowlist, a tag or a branch as much as a SHA."""
    found = pin_faults(repository())
    assert any("pinned to 'abc'" in fault for fault in found), found


def test_the_owner_is_matched_without_regard_to_case() -> None:
    """GitHub resolves the owner and repository case-insensitively."""
    found = pin_faults(repository(publisher=PINNED + SECOND_JOB))
    assert any("pinned to 'main'" in fault for fault in found), found


def test_the_action_path_is_matched_as_written() -> None:
    """The path inside the repository is case-sensitive, so it is not folded.

    A step naming the action under another case resolves to nothing on
    GitHub, so it is no uploader, and the approved pin on it is no proof.
    """
    recased = PINNED.replace(
        "/upload-codescene-coverage@", "/Upload-CodeScene-Coverage@"
    )
    assert recased != PINNED, "the probe's anchor is not in the base"
    found = pin_faults(repository(publisher=recased))
    assert found == [NO_UPLOADER], found


def test_a_commented_out_step_is_not_an_uploader() -> None:
    """With the step gone, a comment naming the approved pin does not count."""
    commented = PINNED.replace(
        f"        uses: leynos/shared-actions/.github/actions/{APPROVED_USES}",
        f"        # uses: leynos/shared-actions/.github/actions/{APPROVED_USES}"
        "        uses: actions/checkout@abc\n",
    )
    assert commented != PINNED, "the probe's anchor is not in the base"
    found = pin_faults(repository(publisher=commented))
    assert found == [NO_UPLOADER], found


@pytest.mark.parametrize(
    ("line", "name"),
    [
        ("installer-checksum: abc\n", "installer-checksum"),
        (
            "archive-checksum: ${{ vars.CODESCENE_CLI_SHA256 }}\n",
            "codescene_cli_sha256",
        ),
    ],
    ids=["input", "variable"],
)
def test_a_deprecated_name_is_found_and_a_comment_is_not(line: str, name: str) -> None:
    """The name is found in a parsed input, and not in a commented-out one."""
    added = PINNED.replace(LAST_INPUT, f"{LAST_INPUT}          {line}")
    commented = PINNED.replace(LAST_INPUT, f"{LAST_INPUT}          # {line}")
    assert added != PINNED, "the probe's anchor is not in the base"
    found = deprecated_mentions(repository(publisher=added), name)
    assert found == [".github/workflows/coverage-main.yml"], found
    found = deprecated_mentions(repository(publisher=commented), name)
    assert found == [], found


def test_a_refresh_workflow_is_found_in_any_case() -> None:
    """The file name is compared case-folded."""
    extra = {"Get-CodeScene-SHA.yml": "on: workflow_dispatch\njobs: {}\n"}
    found = refresh_workflows(repository(publisher=PINNED, extra=extra))
    assert found == [".github/workflows/Get-CodeScene-SHA.yml"], found
