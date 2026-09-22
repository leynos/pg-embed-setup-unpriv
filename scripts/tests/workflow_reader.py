"""Read GitHub Actions workflows the way GitHub does, and refuse the rest.

The coverage-shape contract reasons about every workflow in the
repository, so each way this reader could see less than GitHub runs is a
way the contract passes over a file that violates it. Four are closed
here rather than assumed away.

- PyYAML keeps the last of two duplicate mapping keys and says nothing,
  so a lane could carry one `runs-on` in the discarded half. The loader
  refuses duplicate keys.
- YAML 1.1 reads a bare `on:` as the boolean `True`, and a trigger may
  be written as a scalar, a sequence or a mapping. All six spellings are
  read; anything else is refused rather than read as "no triggers".
- A job calling a local reusable workflow runs that workflow under the
  caller's trigger. `pull_request_closure` follows those calls, matched
  by shape rather than by an enumerated prefix.
- An extension test that is case-sensitive skips a `.YML` workflow in
  silence, so the suffix is folded.

Examples
--------
>>> Workflow.parse("x.yml", "on: [push, pull_request]\\njobs: {}\\n").triggers
{'push': None, 'pull_request': None}
"""

from __future__ import annotations

import typing as typ
from pathlib import Path

import yaml

#: The events that make a workflow a pull-request lane.
PULL_REQUEST_EVENTS: typ.Final = ("pull_request", "pull_request_target")

#: Where a local reusable workflow lives, relative to the repository.
WORKFLOW_PREFIX: typ.Final = ".github/workflows/"

#: The suffixes GitHub loads as workflows, folded.
WORKFLOW_SUFFIXES: typ.Final = (".yml", ".yaml")


class StrictLoader(yaml.SafeLoader):
    """A safe loader that refuses a mapping declaring one key twice."""

    def construct_mapping(
        self, node: yaml.MappingNode, deep: bool = False
    ) -> dict[typ.Hashable, typ.Any]:
        """Build a mapping, failing on the second occurrence of a key.

        Raises
        ------
        yaml.constructor.ConstructorError
            If a key appears twice, naming the key and both lines.
        """
        seen: dict[typ.Hashable, yaml.Mark] = {}
        for key_node, _ in node.value:
            key = self.construct_object(key_node, deep=deep)
            if key in seen:
                raise yaml.constructor.ConstructorError(
                    "while constructing a mapping",
                    seen[key],
                    f"found duplicate key {key!r}",
                    key_node.start_mark,
                )
            seen[key] = key_node.start_mark
        return super().construct_mapping(node, deep=deep)


def triggers(document: dict[typ.Any, typ.Any]) -> dict[str, typ.Any]:
    """Return a workflow's triggers as event name to configuration.

    Examples
    --------
    >>> triggers({True: "push"})
    {'push': None}

    Raises
    ------
    ValueError
        If both the string and the boolean key are present, or the
        trigger is neither a scalar, a sequence nor a mapping.
    """
    keys = [key for key in ("on", True) if key in document]
    if len(keys) > 1:
        msg = "the workflow declares its triggers under both `on` and `true`"
        raise ValueError(msg)
    raw = document[keys[0]] if keys else None
    if isinstance(raw, dict):
        return {str(event): config for event, config in raw.items()}
    if isinstance(raw, str):
        return {raw: None}
    if isinstance(raw, list) and all(isinstance(event, str) for event in raw):
        return dict.fromkeys(raw)
    msg = f"unreadable trigger declaration: {raw!r}"
    raise ValueError(msg)


def local_callee(uses: object) -> str | None:
    """Return the workflow path a job-level `uses` calls locally, if any.

    A leading `./` is stripped and the remainder is local when it names
    a path under the workflow directory; no other prefix is enumerated.

    Examples
    --------
    >>> local_callee("./.github/workflows/build.yml")
    '.github/workflows/build.yml'
    >>> local_callee("leynos/shared-actions/.github/workflows/x.yml@abc") is None
    True
    """
    text = str(uses).strip()
    path = text.removeprefix("./")
    if "@" in path or not path.startswith(WORKFLOW_PREFIX):
        return None
    return path


class Workflow(typ.NamedTuple):
    """One parsed workflow and what it needs to be judged.

    Attributes
    ----------
    path : str
        The path relative to the repository, as a `uses` value names it.
    document : dict
        The parsed document.
    triggers : dict[str, object]
        Event name to its configuration.
    """

    path: str
    document: dict[typ.Any, typ.Any]
    triggers: dict[str, typ.Any]

    @classmethod
    def parse(cls, path: str, text: str) -> Workflow:
        """Parse one workflow's text through the strict loader.

        Raises
        ------
        yaml.YAMLError
            If the text is not YAML, or declares a mapping key twice.
        TypeError
            If the document is not a mapping.
        ValueError
            If its triggers are unreadable.
        """
        document = yaml.load(text, Loader=StrictLoader)
        if not isinstance(document, dict):
            msg = f"{path} does not parse to a mapping"
            raise TypeError(msg)
        return cls(path, document, triggers(document))

    @property
    def on_pull_request(self) -> bool:
        """Return whether the workflow answers a pull-request event."""
        return any(event in self.triggers for event in PULL_REQUEST_EVENTS)

    @property
    def on_push_to_main_only(self) -> bool:
        """Return whether its push trigger names exactly the `main` branch.

        A push trigger naming no branch also answers tag pushes, which
        is how a release workflow would qualify as the trunk publisher.
        """
        push = self.triggers.get("push")
        if not isinstance(push, dict) or "tags" in push:
            return False
        return push.get("branches") == ["main"]

    def jobs(self) -> list[tuple[str, dict[str, typ.Any]]]:
        """Return every job mapping with its identifier."""
        jobs = self.document.get("jobs")
        pairs = jobs.items() if isinstance(jobs, dict) else ()
        return [(str(name), job) for name, job in pairs if isinstance(job, dict)]

    def steps(self) -> list[tuple[str, dict[str, typ.Any]]]:
        """Return every step of every job, with its job identifier."""
        return [
            (name, step)
            for name, job in self.jobs()
            for step in job.get("steps") or []
            if isinstance(step, dict)
        ]

    def callees(self) -> list[str]:
        """Return the local workflow paths this workflow's jobs call."""
        found = (local_callee(job.get("uses", "")) for _, job in self.jobs())
        return [path for path in found if path is not None]


def load_workflows(root: Path) -> list[Workflow]:
    """Return every workflow under the repository's workflow directory.

    Raises
    ------
    AssertionError
        If the directory holds no workflow, which would make every
        assertion over the result vacuous.
    """
    directory = root / WORKFLOW_PREFIX
    found = [
        Workflow.parse(
            path.relative_to(root).as_posix(), path.read_text(encoding="utf-8")
        )
        for path in sorted(directory.iterdir())
        if path.is_file() and path.suffix.lower() in WORKFLOW_SUFFIXES
    ]
    assert found, f"no workflow parsed under {directory}"
    return found


def pull_request_closure(workflows: list[Workflow]) -> list[Workflow]:
    """Return every workflow a pull request runs, following local calls.

    Examples
    --------
    >>> caller = Workflow.parse(
    ...     ".github/workflows/a.yml",
    ...     "on: pull_request\\njobs: {x: {uses: ./.github/workflows/b.yml}}\\n",
    ... )
    >>> callee = Workflow.parse(
    ...     ".github/workflows/b.yml", "on: workflow_call\\njobs: {}\\n"
    ... )
    >>> [flow.path for flow in pull_request_closure([caller, callee])]
    ['.github/workflows/a.yml', '.github/workflows/b.yml']

    Raises
    ------
    AssertionError
        If a local call names a workflow that is not in the list, since
        the closure would otherwise stop at it in silence.
    """
    by_path = {flow.path: flow for flow in workflows}
    pending = [flow.path for flow in workflows if flow.on_pull_request]
    reached: list[str] = []
    while pending:
        path = pending.pop(0)
        if path in reached:
            continue
        assert path in by_path, f"a pull-request lane calls missing {path}"
        reached.append(path)
        pending.extend(by_path[path].callees())
    return [by_path[path] for path in reached]


def scalars(node: object) -> list[str]:
    """Return every key and scalar value in a parsed document, as text.

    Examples
    --------
    >>> scalars({"env": {"A": "${{ secrets.B }}"}, "on": ["push"]})
    ['env', 'A', '${{ secrets.B }}', 'on', 'push']
    """
    if isinstance(node, dict):
        return [
            text
            for key, value in node.items()
            for text in [*scalars(key), *scalars(value)]
        ]
    if isinstance(node, list):
        return [text for item in node for text in scalars(item)]
    return [] if node is None else [str(node)]
