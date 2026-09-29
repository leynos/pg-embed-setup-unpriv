"""Read GitHub Actions workflows the way GitHub does, and refuse the rest.

The runner-placement and timeout contracts reason about every workflow in the
repository, so each way this reader could see less than GitHub runs is a
way a contract passes over a file that violates it. Three are closed here
rather than assumed away. (The CV-005 CodeScene contract, which also followed
local reusable-workflow calls, now runs from the shared library through
`make test-workflow-contracts`.)

- PyYAML keeps the last of two duplicate mapping keys and says nothing,
  so a lane could carry one `runs-on` in the discarded half. The loader
  refuses duplicate keys.
- YAML 1.1 reads a bare `on:` as the boolean `True`, and a trigger may
  be written as a scalar, a sequence or a mapping. All six spellings are
  read; anything else is refused rather than read as "no triggers".
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

#: Where GitHub reads a repository's workflows, relative to the repository.
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
    return _event_map(_trigger_declaration(document))


def _trigger_declaration(document: dict[typ.Any, typ.Any]) -> object:
    """Return the value under whichever trigger key the document used.

    Raises
    ------
    ValueError
        If both the string and the boolean key are present.
    """
    keys = [key for key in ("on", True) if key in document]
    if len(keys) > 1:
        msg = "the workflow declares its triggers under both `on` and `true`"
        raise ValueError(msg)
    return document[keys[0]] if keys else None


def _event_map(raw: object) -> dict[str, typ.Any]:
    """Return a trigger declaration as event name to configuration.

    Raises
    ------
    ValueError
        If the declaration is neither a scalar, a sequence of names nor a
        mapping.
    """
    if isinstance(raw, dict):
        return {str(event): config for event, config in raw.items()}
    events = [raw] if isinstance(raw, str) else raw
    if isinstance(events, list) and all(isinstance(name, str) for name in events):
        return dict.fromkeys(events)
    msg = f"unreadable trigger declaration: {raw!r}"
    raise ValueError(msg)


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
