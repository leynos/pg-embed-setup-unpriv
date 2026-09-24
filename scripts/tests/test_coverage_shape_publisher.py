"""Drive the trunk publisher's rules past the hazard table.

`test_coverage_shape_probes` holds the one-edit hazards. These cases need
more than one edit to build: a second publisher, a publisher that
bypasses the uploader action, a guard carrying an operator as text, and
the generator guards the trunk push satisfies. Each starts from the same
constructed repository the probes suite uses.
"""

from __future__ import annotations

import typing as typ

import pytest
from coverage_shape_rules import publisher_faults
from test_coverage_shape_probes import GUARD, PUBLISHER, repository


def test_a_second_publisher_is_named() -> None:
    """Two upload steps race to write one baseline."""
    found = publisher_faults(repository(callee=PUBLISHER))
    assert any("expected exactly one upload step" in fault for fault in found), found


def test_an_operator_inside_a_string_literal_is_not_one() -> None:
    """A `||` quoted in the guard is text, so the guard still holds.

    The narrow half of the disjunction probe: refusing every condition
    that contains the two characters would pass that probe while
    rejecting guards that confine the upload perfectly well.
    """
    guarded = GUARD + " && github.actor != 'a || b'"
    found = publisher_faults(repository(publisher=PUBLISHER.replace(GUARD, guarded)))
    assert found == [], f"a quoted operator was read as a disjunction: {found}"


#: Push workflows that publish without the uploader action.
BYPASSES: typ.Final = {
    "the CLI": "      - run: cs-coverage upload lcov.info\n",
    "the host": "      - run: curl -X POST https://api.codescene.io/v2/projects\n",
    "the token": "      - run: echo ${{ secrets.CS_ACCESS_TOKEN }}\n",
}


@pytest.mark.parametrize("step", BYPASSES.values(), ids=BYPASSES.keys())
def test_a_publisher_bypassing_the_uploader_action_is_named(step: str) -> None:
    """A second trunk publisher need not use the action to race the first."""
    bypass = (
        "on:\n  push:\n    branches: [main]\njobs:\n  publish:\n"
        "    runs-on: ubuntu-latest\n    steps:\n" + step
    )
    found = publisher_faults(repository(callee=bypass))
    assert any("callee.yml" in fault for fault in found), found


#: Generator guards the publisher satisfies on a push to main.
TRUNK_GENERATORS: typ.Final = {
    "guarded on the trunk ref": (
        "      - if: github.ref == 'refs/heads/main'\n"
        "        uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n"
    ),
    "guarded on a leg of the publisher's matrix": (
        "      - if: matrix.shard == '1'\n"
        "        uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n"
    ),
}


@pytest.mark.parametrize(
    "generator", TRUNK_GENERATORS.values(), ids=TRUNK_GENERATORS.keys()
)
def test_a_generator_the_trunk_run_satisfies_is_accepted(generator: str) -> None:
    """The narrow half: generation is judged in the publisher's own context."""
    publisher = PUBLISHER.replace(
        "      - uses: leynos/shared-actions/.github/actions/generate-coverage@abc\n",
        generator,
    ).replace(
        "    runs-on: ubuntu-latest\n",
        "    runs-on: ubuntu-latest\n    strategy:\n      matrix:\n        shard: ['1']\n",
    )
    found = publisher_faults(repository(publisher=publisher))
    assert found == [], f"a generator that runs on the trunk push was refused: {found}"
