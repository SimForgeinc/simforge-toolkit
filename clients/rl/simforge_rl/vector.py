"""Vectorized scenario-set environments: one session per scenario per sub-env."""

from __future__ import annotations

from typing import Any, Callable, Literal, Mapping, Sequence

import gymnasium as gym
from gymnasium.vector import AsyncVectorEnv, AutoresetMode, SyncVectorEnv

from .scenarios import ScenarioSet, ScenarioSetEnv
from .transport import EnvTransport

Sources = Mapping[str, Callable[[], EnvTransport] | str]


class _Factory:
    def __init__(self, scenario_set: ScenarioSet, sources: Sources | None, wrappers: Any, env_kwargs: dict[str, Any]) -> None:
        self.scenario_set, self.sources, self.wrappers, self.env_kwargs = scenario_set, sources, wrappers, env_kwargs

    def __call__(self) -> gym.Env[Any, Any]:
        env: gym.Env[Any, Any] = ScenarioSetEnv(self.scenario_set, self.sources, **self.env_kwargs)
        return self.wrappers(env) if self.wrappers is not None else env


def make_vector_env(
    scenario_set: ScenarioSet,
    transports: Sources | Sequence[Sources] | None,
    num_envs: int,
    *,
    vectorization: Literal["sync", "async"] = "async",
    wrappers: Callable[[gym.Env[Any, Any]], gym.Env[Any, Any]] | None = None,
    **env_kwargs: Any,
) -> gym.vector.VectorEnv[Any, Any, Any]:
    """``num_envs`` sub-envs with same-step autoreset; ``transports`` may give one mapping per sub-env."""
    if num_envs < 1:
        raise ValueError("num_envs must be at least 1")
    if transports is None or isinstance(transports, Mapping):
        per_env: list[Sources | None] = [None if transports is None else dict(transports)] * num_envs
    else:
        per_env = [dict(t) for t in transports]
        if len(per_env) != num_envs:
            raise ValueError(f"{len(per_env)} transport mappings for {num_envs} sub-envs")
    fns = [_Factory(scenario_set, sources, wrappers, env_kwargs) for sources in per_env]
    if vectorization == "sync":
        return SyncVectorEnv(fns, autoreset_mode=AutoresetMode.SAME_STEP)
    if vectorization == "async":
        # Spawned workers inherit no runtime reader threads, locks or sockets from this process.
        return AsyncVectorEnv(fns, autoreset_mode=AutoresetMode.SAME_STEP, context="spawn")
    raise ValueError(f"unknown vectorization {vectorization!r}")
