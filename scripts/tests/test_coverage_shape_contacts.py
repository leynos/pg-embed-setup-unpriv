"""Drive the pull-request contact rule with every route to CodeScene.

A pull-request lane, or anything it calls, must not reference the token,
name the host, run the CLI, forward every secret or use the uploader
action. Each case adds one such route to the constructed repository the
probes suite uses and asserts the rule names it.
"""

from __future__ import annotations

import typing as typ

import pytest
from coverage_shape_rules import codescene_contacts
from test_coverage_shape_probes import (
    CALLEE,
    CI,
    DECLARES,
    repository,
    with_job,
    with_step,
)

CONTACTS: typ.Final = {
    "a called workflow, reached through ./": (
        with_job("probe:\n  uses: ./.github/workflows/callee.yml"),
        [
            "callee.yml: references the access token",
            "callee.yml: names the CodeScene host",
        ],
    ),
    "a called workflow, reached without ./": (
        with_job("probe:\n  uses: .github/workflows/callee.yml"),
        ["callee.yml: references the access token"],
    ),
    "a called workflow, reached through $/": (
        with_job("probe:\n  uses: $/.github/workflows/callee.yml"),
        ["callee.yml: references the access token"],
    ),
    "the token in a run body, lower case": (
        with_step("- run: echo ${{ secrets.cs_access_token }}"),
        ["ci.yml: references the access token"],
    ),
    "the token as an action input": (
        with_step(
            "- uses: some/action@abc\n  with:\n    key: ${{ secrets.CS_ACCESS_TOKEN }}"
        ),
        ["ci.yml: references the access token"],
    ),
    "the token at workflow scope": (
        "env:\n  X: ${{ secrets.CS_ACCESS_TOKEN }}\n" + CI,
        ["ci.yml: references the access token"],
    ),
    "the token forwarded by name": (
        with_job(
            "call:\n  uses: o/r/.github/workflows/x.yml@abc\n  secrets:\n    K: ${{ secrets.CS_ACCESS_TOKEN }}"
        ),
        ["ci.yml: references the access token"],
    ),
    "every secret forwarded": (
        with_job("call:\n  uses: o/r/.github/workflows/x.yml@abc\n  secrets: inherit"),
        ["ci.yml:call: forwards every secret"],
    ),
    "the host in the workflow's default shell": (
        "defaults:\n  run:\n    shell: bash -c 'curl -s https://codescene.io; {0}'\n"
        + CI,
        ["ci.yml: names the CodeScene host"],
    ),
    "a callee declaring the token as a secret it accepts": (
        with_job("probe:\n  uses: ./.github/workflows/declares.yml"),
        ["declares.yml: references the access token"],
    ),
    "the host, with no token or action": (
        with_step("- run: curl https://api.codescene.io/v2/projects"),
        ["ci.yml: names the CodeScene host"],
    ),
    "the CodeScene CLI": (
        with_step("- run: cs-coverage check lcov.info"),
        ["ci.yml: runs the CodeScene CLI"],
    ),
    "the uploader action": (
        with_step(
            "- uses: leynos/shared-actions/.github/actions/upload-codescene-coverage@abc"
        ),
        ["ci.yml:test: uses the uploader action"],
    ),
}


@pytest.mark.parametrize(("ci", "expected"), CONTACTS.values(), ids=CONTACTS.keys())
def test_every_route_to_codescene_is_named(ci: str, expected: list[str]) -> None:
    """Each way a pull-request lane could reach the service is reported."""
    found = codescene_contacts(
        repository(ci=ci, callee=CALLEE, extra={"declares.yml": DECLARES})
    )
    missing = [
        fragment
        for fragment in expected
        if not any(fragment in offence for offence in found)
    ]
    assert not missing, f"{missing} not among {found}"
