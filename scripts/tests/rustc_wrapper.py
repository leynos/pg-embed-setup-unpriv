"""Judge whether the sccache-sensitive CI steps run plain rustc.

`setup-rust` exports `RUSTC_WRAPPER` for the whole job. Three steps must
override it with an empty value (which counts as unset): the unprivileged
test step and the cross-platform unprivileged-surface step, whose `ui` tests
run nested cargo builds that slow down under the wrapper, and the root step,
where `sudo -E` would send root's rustc through a server owned by the runner
user.
"""

from __future__ import annotations

import re

from workflow_reader import Workflow

UNPRIVILEGED_STEP = "Test with all features (unprivileged)"
SURFACE_STEP = "Test unprivileged surface"
ROOT_STEP = "Install cargo-nextest and test (root)"

#: The `run_as_root` shell function: its opening line to the closing brace at
#: the same indent, so an assignment elsewhere in the step cannot count.
_ROOT_FUNCTION = re.compile(
    r"^(?P<indent>[ \t]*)run_as_root\s*\(\s*\)\s*\{(?P<body>.*?)^(?P=indent)\}",
    re.MULTILINE | re.DOTALL,
)

#: The `sudo -E env \` line and the continued `NAME=value \` lines after it.
_SUDO_ENV = re.compile(
    r"^[ \t]*sudo[ \t]+-E[ \t]+env[ \t]*\\[ \t]*\n"
    r"(?P<arguments>(?:^[ \t]+[A-Za-z_][A-Za-z0-9_]*=.*\\[ \t]*\n)*)",
    re.MULTILINE,
)

#: An empty `RUSTC_WRAPPER=` argument: empty means unset to cargo.
_EMPTY_ASSIGNMENT = re.compile(r"^[ \t]+RUSTC_WRAPPER=[ \t]*\\[ \t]*$", re.MULTILINE)


def _root_command_empties_wrapper(run: str) -> bool:
    """Return whether `run_as_root` passes an empty `RUSTC_WRAPPER` to `sudo -E env`."""
    helper = _ROOT_FUNCTION.search(run)
    if helper is None:
        return False
    sudo_env = _SUDO_ENV.search(helper.group("body"))
    return sudo_env is not None and bool(_EMPTY_ASSIGNMENT.search(sudo_env.group("arguments")))


def wrapper_faults(workflow: Workflow) -> list[str]:
    """Return what is wrong with how the three steps handle the wrapper.

    Parameters
    ----------
    workflow : Workflow
        The parsed workflow to judge.

    Returns
    -------
    list[str]
        One message per fault, each naming the step at fault; empty when every
        step runs plain `rustc`.

    Examples
    --------
    >>> wrapper_faults(Workflow("x.yml", {}, {}))  # doctest: +NORMALIZE_WHITESPACE
    ['no step named Test with all features (unprivileged)',
     'no step named Test unprivileged surface',
     'no step named Install cargo-nextest and test (root)']
    """
    steps = {str(step.get("name")): step for _job, step in workflow.steps()}
    faults: list[str] = []
    for name in (UNPRIVILEGED_STEP, SURFACE_STEP):
        step = steps.get(name)
        if step is None:
            faults.append(f"no step named {name}")
        elif (step.get("env") or {}).get("RUSTC_WRAPPER", None) != "":
            faults.append(f"{name} does not set RUSTC_WRAPPER to an empty value")
    root = steps.get(ROOT_STEP)
    if root is None:
        faults.append(f"no step named {ROOT_STEP}")
    elif not _root_command_empties_wrapper(str(root.get("run", ""))):
        faults.append(f"{ROOT_STEP} does not empty RUSTC_WRAPPER for the root command")
    return faults
