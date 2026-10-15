"""Scenario sets as task distributions: seeded sampling of (scenario, episode seed) per reset."""

from __future__ import annotations

import json
import os
import tempfile
from collections import OrderedDict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Mapping, Sequence

import gymnasium as gym
import numpy as np

from .env import SimForgeEnv
from .runtime import RuntimeClient
from .server import EnvServer, _xodr_sha256, template_workspace
from .transport import EnvTransport, RuntimeEnvTransport, SocketTransport

SEED_SPACE = 2**31 - 1
SET_SCHEMA = "simforge-rl.scenario-set/v1"
TASKS = Path(__file__).parent / "tasks"


@dataclass(frozen=True)
class Scenario:
    """One task: exactly one of a workspace, a template or a running endpoint, plus weight and seeds.

    A template compiles on the best site of its map, or on ``site`` (a ``simforge sites match``
    id of that map version).
    """

    name: str
    workspace: str | None = None
    template: str | None = None
    endpoint: str | None = None
    weight: float = 1.0
    seeds: tuple[int, ...] = ()
    site: str | None = None

    def __post_init__(self) -> None:
        if not self.name:
            raise ValueError("a scenario needs a name")
        if sum(x is not None for x in (self.workspace, self.template, self.endpoint)) != 1:
            raise ValueError(f"scenario {self.name!r} needs exactly one of workspace, template or endpoint")
        if self.site is not None and self.template is None:
            raise ValueError(f"scenario {self.name!r}: only a template takes a site")
        object.__setattr__(self, "seeds", tuple(int(s) for s in self.seeds))


@dataclass(frozen=True)
class MapPin:
    """The installed map a set's templates compile on when no map directory is given."""

    name: str
    version: str
    xodr_sha256: str


@dataclass(frozen=True)
class ScenarioSet:
    """A weighted task distribution; sampling is a pure function of the generator."""

    scenarios: tuple[Scenario, ...]
    map: MapPin | None = None
    probabilities: np.ndarray = field(init=False, repr=False, compare=False)

    def __post_init__(self) -> None:
        if not self.scenarios:
            raise ValueError("a scenario set needs at least one scenario")
        names = [s.name for s in self.scenarios]
        if len(set(names)) != len(names):
            raise ValueError(f"duplicate scenario names in {names}")
        weights = np.array([s.weight for s in self.scenarios], dtype=np.float64)
        if not np.all(np.isfinite(weights)) or np.any(weights < 0) or weights.sum() <= 0:
            raise ValueError(f"scenario weights must be finite, non-negative and not all zero: {weights.tolist()}")
        object.__setattr__(self, "probabilities", weights / weights.sum())

    @classmethod
    def of(cls, scenarios: Sequence[Scenario], map: MapPin | None = None) -> ScenarioSet:
        return cls(tuple(scenarios), map)

    @classmethod
    def bundled(cls, name: str) -> ScenarioSet:
        """A task set shipped with this package for a released map (:func:`bundled_sets` names them)."""
        path = TASKS / f"{name}.json"
        if not path.is_file():
            raise KeyError(f"no bundled scenario set {name!r}; bundled: {bundled_sets()}")
        return cls.load(path)

    @classmethod
    def from_export(
        cls,
        source: str | os.PathLike[str],
        *,
        split: str | None = "train",
        map_name: str | None = None,
        cache_dir: str | os.PathLike[str] | None = None,
        binary: str | os.PathLike[str] | None = None,
    ) -> ScenarioSet:
        """Every scenario of a ``simforge.dataset-export/v1`` export (of ``split``, on ``map_name``) as a task.

        ``source`` is a downloaded export folder, or a cloud export id that ``simforge datasets
        pull`` downloads first. Episodes replay each package's simulated input on the installed
        map its OpenDRIVE digest names (see :mod:`simforge_rl.exports`).
        """
        from .exports import export_scenarios

        return cls.of(export_scenarios(source, split=split, map_name=map_name, cache_dir=cache_dir, binary=binary))

    @classmethod
    def load(cls, path: str | os.PathLike[str]) -> ScenarioSet:
        """Read a ``simforge-rl.scenario-set/v1`` file; relative paths resolve against it."""
        base = Path(path).resolve().parent
        document = json.loads(Path(path).read_text())
        if not isinstance(document, dict) or document.get("schema") != SET_SCHEMA:
            raise ValueError(f"{path} is not a {SET_SCHEMA} document")
        unknown = set(document) - {"schema", "scenarios", "map"}
        if unknown:
            raise ValueError(f"{path}: unknown fields {sorted(unknown)}")
        pin = document.get("map")
        if pin is not None and set(pin) != {"name", "version", "xodrSha256"}:
            raise ValueError(f"{path}: map needs exactly name, version and xodrSha256, found {sorted(pin)}")
        scenarios = []
        for entry in document.get("scenarios", []):
            unknown = set(entry) - {"name", "workspace", "template", "endpoint", "weight", "seeds", "site"}
            if unknown:
                raise ValueError(f"{path}: unknown scenario fields {sorted(unknown)}")
            where = {k: os.path.normpath(base / entry[k]) for k in ("workspace", "template", "endpoint") if entry.get(k) is not None}
            scenarios.append(Scenario(entry["name"], weight=float(entry.get("weight", 1.0)), seeds=tuple(entry.get("seeds", ())), site=entry.get("site"), **where))
        return cls.of(scenarios, None if pin is None else MapPin(pin["name"], pin["version"], pin["xodrSha256"]))

    def save(self, path: str | os.PathLike[str]) -> None:
        entries = []
        for s in self.scenarios:
            entry: dict[str, Any] = {"name": s.name, "weight": s.weight}
            entry.update({k: getattr(s, k) for k in ("workspace", "template", "endpoint") if getattr(s, k) is not None})
            if s.seeds:
                entry["seeds"] = list(s.seeds)
            if s.site is not None:
                entry["site"] = s.site
            entries.append(entry)
        document: dict[str, Any] = {"schema": SET_SCHEMA, "scenarios": entries}
        if self.map is not None:
            document["map"] = {"name": self.map.name, "version": self.map.version, "xodrSha256": self.map.xodr_sha256}
        Path(path).write_text(json.dumps(document, indent=2) + "\n")

    def index(self, name: str) -> int:
        for i, scenario in enumerate(self.scenarios):
            if scenario.name == name:
                return i
        raise KeyError(f"no scenario {name!r} in {[s.name for s in self.scenarios]}")

    def sample(self, rng: np.random.Generator) -> tuple[Scenario, int]:
        """Draw a scenario by weight and an episode seed (its fixed list, else uniform)."""
        scenario = self.scenarios[int(rng.choice(len(self.scenarios), p=self.probabilities))]
        if scenario.seeds:
            return scenario, int(scenario.seeds[int(rng.integers(len(scenario.seeds)))])
        return scenario, int(rng.integers(SEED_SPACE))


def bundled_sets() -> list[str]:
    """The names of the scenario sets shipped with this package, one per released map."""
    return sorted(p.stem for p in TASKS.glob("*.json"))


def _workspaces(scenario_set: ScenarioSet, root: Path, map_dir: str | None) -> dict[str, str]:
    """Workspace per template/workspace scenario: a template compiles on ``map_dir``, else on the set's pinned installed map."""
    pin = scenario_set.map
    if pin is not None and map_dir is not None and any(s.template is not None for s in scenario_set.scenarios):
        found = _xodr_sha256(Path(map_dir))
        if found != pin.xodr_sha256:
            raise ValueError(
                f"this task set was verified on {pin.name} {pin.version} (OpenDRIVE {pin.xodr_sha256[:12]}), but {map_dir} holds "
                f"OpenDRIVE {found[:12]}: pass that version's map, or none to use the installed {pin.name}@{pin.version}"
            )
    out = {}
    for i, s in enumerate(scenario_set.scenarios):
        if s.template is not None:
            if map_dir is None and pin is None:
                raise ValueError(f"scenario {s.name!r} is a template: pass map_dir, or pin the set to an installed map")
            workspace = (
                template_workspace(s.template, map_dir, root / f"w{i}", site=s.site)
                if map_dir is not None
                else template_workspace(s.template, None, root / f"w{i}", map_id=pin.name, xodr_sha256=pin.xodr_sha256, site=s.site)
            )
            out[s.name] = os.fspath(workspace)
        elif s.workspace is not None:
            out[s.name] = s.workspace
    return out


class ScenarioServers:
    """``replicas`` ``env serve`` processes per workspace/template scenario; endpoints pass through."""

    def __init__(
        self,
        scenario_set: ScenarioSet,
        *args: str,
        sessions: int = 8,
        replicas: int = 1,
        map_dir: str | os.PathLike[str] | None = None,
        socket_dir: str | os.PathLike[str] | None = None,
        binary: str | os.PathLike[str] | None = None,
        env: Mapping[str, str] | None = None,
    ) -> None:
        if replicas < 1:
            raise ValueError("replicas must be at least 1")
        self.scenario_set = scenario_set
        self._tmp = tempfile.TemporaryDirectory(prefix="sfrl-")
        root = Path(socket_dir) if socket_dir is not None else Path(self._tmp.name)
        extra = ("--map-dir", os.fspath(map_dir)) if map_dir is not None else ()
        self.servers: dict[str, list[EnvServer]] = {}
        try:
            workspaces = _workspaces(scenario_set, Path(self._tmp.name), None if map_dir is None else os.fspath(map_dir))
            for i, scenario in enumerate(scenario_set.scenarios):
                if scenario.name not in workspaces:
                    continue
                group = self.servers.setdefault(scenario.name, [])
                for r in range(replicas):
                    group.append(EnvServer(
                        workspaces[scenario.name], root / f"s{i}-{r}.sock", *args, *extra, "--max-sessions", str(sessions), binary=binary, env=env
                    ))
        except BaseException:
            self.close()
            raise

    def socket_sets(self, count: int) -> list[dict[str, str]]:
        """``count`` scenario-to-socket mappings, spreading consumers round-robin over replicas."""
        return [
            {s.name: s.endpoint if s.endpoint is not None else os.fspath(self.servers[s.name][k % len(self.servers[s.name])].socket)
             for s in self.scenario_set.scenarios}
            for k in range(count)
        ]

    @property
    def sockets(self) -> dict[str, str]:
        return self.socket_sets(1)[0]

    def close(self) -> None:
        for group in self.servers.values():
            for server in group:
                server.close()
        self.servers.clear()
        self._tmp.cleanup()

    def __enter__(self) -> ScenarioServers:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


class RuntimeSessions:
    """Picklable per-scenario transport factories over one ``simforge-runtime serve`` per process."""

    def __init__(
        self,
        scenario_set: ScenarioSet,
        *args: str,
        binary: str | os.PathLike[str] | None = None,
        map_dir: str | os.PathLike[str] | None = None,
        workdir: str | os.PathLike[str],
    ) -> None:
        if any(s.endpoint is not None for s in scenario_set.scenarios):
            raise ValueError("runtime sessions create their own envs: endpoint scenarios are not allowed")
        extra = ("--map-dir", os.path.abspath(map_dir)) if map_dir is not None else ()
        workspaces = _workspaces(scenario_set, Path(workdir), None if map_dir is None else os.fspath(map_dir))
        self.argv = {name: [os.path.abspath(ws), *args, *extra] for name, ws in workspaces.items()}
        self.binary = None if binary is None else os.fspath(binary)
        self._client: RuntimeClient | None = None

    def __getstate__(self) -> dict[str, Any]:
        return {**self.__dict__, "_client": None}

    def client(self) -> RuntimeClient:
        if self._client is None or not self._client.alive:
            self._client = RuntimeClient.spawn(self.binary)
        return self._client

    def transports(self) -> dict[str, Callable[[], EnvTransport]]:
        return {name: _RuntimeFactory(self, name) for name in self.argv}

    def close(self) -> None:
        """Stop this process's runtime; worker copies' runtimes exit on EOF when their worker ends."""
        client, self._client = self._client, None
        if client is not None:
            client.close()

    def __enter__(self) -> RuntimeSessions:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


@dataclass
class _RuntimeFactory:
    sessions: RuntimeSessions
    name: str

    def __call__(self) -> EnvTransport:
        return RuntimeEnvTransport(self.sessions.client(), self.sessions.argv[self.name])


class ScenarioSetEnv(gym.Env[dict[str, Any], np.ndarray]):
    """Each reset samples a scenario and seed from ``np_random``; every scenario must share spaces.

    With ``max_open=None`` every scenario's session opens here. With ``max_open=k`` a session
    opens when its scenario is first drawn and at most ``k`` stay open (least recently used
    closes first), so a set of thousands of exported scenarios needs only ``k`` sessions;
    spaces are then checked as each session opens.
    """

    metadata: dict[str, Any] = {"render_modes": []}

    def __init__(
        self,
        scenario_set: ScenarioSet,
        transports: Mapping[str, Callable[[], EnvTransport] | str] | None = None,
        *,
        max_open: int | None = None,
        **env_kwargs: Any,
    ) -> None:
        super().__init__()
        if max_open is not None and max_open < 1:
            raise ValueError("max_open must be at least 1")
        self.scenario_set = scenario_set
        self.max_open = max_open
        self.env_kwargs = env_kwargs
        self.sources: dict[str, Callable[[], EnvTransport] | str] = {}
        for scenario in scenario_set.scenarios:
            source = transports[scenario.name] if transports is not None else scenario.endpoint
            if source is None:
                raise ValueError(f"scenario {scenario.name!r} has no endpoint: pass transports")
            self.sources[scenario.name] = source
        self.envs: OrderedDict[str, SimForgeEnv] = OrderedDict()
        first_name = scenario_set.scenarios[0].name
        try:
            first = self._open(first_name)
            self.observation_space = first.observation_space
            self.action_space = first.action_space
            if max_open is None:
                for scenario in scenario_set.scenarios[1:]:
                    self._open(scenario.name)
        except BaseException:
            self.close()
            raise
        self.current: SimForgeEnv | None = None

    def _open(self, name: str) -> SimForgeEnv:
        """The scenario's session, opened on first use; evicts the least recently used past ``max_open``."""
        if name in self.envs:
            self.envs.move_to_end(name)
            return self.envs[name]
        source = self.sources[name]
        transport = SocketTransport(source) if isinstance(source, (str, os.PathLike)) else source()
        env = SimForgeEnv(transport, **self.env_kwargs)
        if self.envs and (env.observation_space != self.observation_space or env.action_space != self.action_space):
            env.close()
            raise ValueError(f"scenario {name!r} has different spaces than {self.scenario_set.scenarios[0].name!r}")
        self.envs[name] = env
        while self.max_open is not None and len(self.envs) > self.max_open:
            self.envs.popitem(last=False)[1].close()
        return env

    def reset(self, *, seed: int | None = None, options: Mapping[str, Any] | None = None) -> tuple[dict[str, Any], dict[str, Any]]:
        """``options["scenario_name"]``/``["episode_seed"]`` pin the draw; others pass through."""
        super().reset(seed=seed)
        options = dict(options or {})
        scenario, episode_seed = self.scenario_set.sample(self.np_random)
        if "scenario_name" in options:
            scenario = self.scenario_set.scenarios[self.scenario_set.index(options.pop("scenario_name"))]
        episode_seed = int(options.pop("episode_seed", episode_seed))
        self.current = self._open(scenario.name)
        observation, info = self.current.reset(options={**options, "episode_seed": episode_seed})
        info["scenario"] = scenario.name
        info["episode_seed"] = episode_seed
        return observation, info

    def step(self, action: Any) -> tuple[dict[str, Any], float, bool, bool, dict[str, Any]]:
        if self.current is None:
            raise RuntimeError("reset before step")
        return self.current.step(action)

    def close(self) -> None:
        for env in self.envs.values():
            env.close()
        self.envs.clear()
