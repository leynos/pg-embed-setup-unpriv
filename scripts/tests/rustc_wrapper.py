"""Judge whether the two sccache-sensitive CI steps run plain rustc.

`setup-rust` exports `RUSTC_WRAPPER` for the whole job. Two steps must
override it with an empty value (which counts as unset): the unprivileged
test step, whose `ui` tests run nested cargo builds that slow down under the
wrapper, and the root step, where `sudo -E` would send root's rustc through a
server owned by the runner user.
"""

from __future__ import annotations

import re

from workflow_reader import Workflow

UNPRIVILEGED_STEP = "Test with all features (unprivileged)"
ROOT_STEP = "Install cargo-nextest and test (root)"

#: `RUSTC_WRAPPER=` followed by whitespace or a line continuation, so only an
#: empty assignment inside the `sudo env` argument list counts.
_EMPTY_ASSIGNMENT = re.compile(r"^\s*RUSTC_WRAPPER=\s*\\?\s*$", re.MULTILINE)


def wrapper_faults(workflow: Workflow) -> list[str]:
    """Return what is wrong with how the two steps handle the wrapper.

    >>> wrapper_faults(Workflow("x.yml", {}, {}))
    ['no step named Test with all features (unprivileged)', 'no step named Install cargo-nextest and test (root)']
    """
    steps = {str(step.get("name")): step for _job, step in workflow.steps()}
    faults: list[str] = []
    unprivileged = steps.get(UNPRIVILEGED_STEP)
    if unprivileged is None:
        faults.append(f"no step named {UNPRIVILEGED_STEP}")
    elif (unprivileged.get("env") or {}).get("RUSTC_WRAPPER", None) != "":
        faults.append(f"{UNPRIVILEGED_STEP} does not set RUSTC_WRAPPER to an empty value")
    root = steps.get(ROOT_STEP)
    if root is None:
        faults.append(f"no step named {ROOT_STEP}")
    elif not _EMPTY_ASSIGNMENT.search(str(root.get("run", ""))):
        faults.append(f"{ROOT_STEP} does not empty RUSTC_WRAPPER for the root command")
    return faults
