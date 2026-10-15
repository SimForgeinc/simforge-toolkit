"""Gymnasium VectorEnv over one runtime ``env.batch`` (state observations, in-runtime CPU parallelism)."""

from __future__ import annotations

from typing import Any, Sequence

import gymnasium as gym
import numpy as np
from gymnasium.vector import AutoresetMode
from gymnasium.vector.utils import batch_space

from .env import ActionMode, action_space_for, encode_row, state_space, unpack_state
from .rewards import RewardFn, native
from .runtime import BatchResults, RuntimeBatch, RuntimeClient
from .scenarios import SEED_SPACE
from .wire import PROTOCOL, EnvServeError, ProtocolError


class SimForgeBatchVectorEnv(gym.vector.VectorEnv[Any, Any, Any]):
    """``num_envs`` episodes of one workspace; an ended env is reset (masked) on the next step."""

    metadata: dict[str, Any] = {"autoreset_mode": AutoresetMode.NEXT_STEP}

    def __init__(
        self,
        runtime: RuntimeClient,
        argv: Sequence[str],
        num_envs: int,
        *,
        shards: int | None = None,
        cwd: str | None = None,
        action_mode: ActionMode = "setpoint",
        reward_fn: RewardFn = native,
        info_channel: bool = True,
    ) -> None:
        self.runtime = runtime
        self.batch: RuntimeBatch | None = runtime.batch_create(argv, num_envs, shards=shards, cwd=cwd)
        try:
            self.hello = self.batch.hello
            if self.hello.get("protocol") != PROTOCOL:
                raise ProtocolError(f"env.batch hello speaks {self.hello.get('protocol')!r}, not {PROTOCOL}")
            self.num_envs = num_envs
            self.action_mode = action_mode
            self.reward_fn = reward_fn
            self.info_channel = info_channel
            self._slot = {name: index for index, name in enumerate(self.hello["actionFields"])}
            self._width = int(self.hello["actionWidth"])
            self._state = dict(state_space(self.hello).spaces)
            self.single_observation_space = gym.spaces.Dict(self._state)
            self.single_action_space = action_space_for(action_mode)
            self.observation_space = batch_space(self.single_observation_space, num_envs)
            self.action_space = batch_space(self.single_action_space, num_envs)
        except BaseException:
            self.close()
            raise
        self._ended = np.zeros(num_envs, dtype=bool)
        self._rng = np.random.default_rng()

    def _rows(self, op: str, results: BatchResults) -> None:
        if results.failed:
            codes = {k: results.rows[k].header.get("code") for k in results.failed}  # type: ignore[union-attr]
            error = EnvServeError(op, "batch_rows_failed", f"envs {list(results.failed)} failed: {codes}", codes)
            error.results = results  # type: ignore[attr-defined]
            raise error

    def _seeds(self, mask: np.ndarray) -> list[int | None]:
        return [int(self._rng.integers(SEED_SPACE)) if m else None for m in mask]

    def reset(self, *, seed: int | list[int] | None = None, options: dict[str, Any] | None = None) -> tuple[dict[str, np.ndarray], dict[str, Any]]:
        """Reset every env; ``seed`` reseeds the episode-seed stream (a list gives each env's seed)."""
        if self.batch is None:
            raise RuntimeError("the batch is closed")
        if options:
            raise ValueError(f"SimForgeBatchVectorEnv.reset takes no options, got {sorted(options)}")
        if isinstance(seed, list):
            if len(seed) != self.num_envs:
                raise ValueError(f"{len(seed)} seeds for {self.num_envs} envs")
            seeds: list[int | None] = [int(s) for s in seed]
            self._rng = np.random.default_rng(seeds)
        else:
            if seed is not None:
                self._rng = np.random.default_rng(seed)
            seeds = self._seeds(np.ones(self.num_envs, dtype=bool))
        results = self.runtime.batch_reset(self.batch, seeds)
        self._rows("reset", results)
        self._ended[:] = False
        obs, infos = self._collect(results.rows)
        infos["episode_seed"] = np.array(seeds)
        return obs, infos

    def step(self, actions: Any) -> tuple[dict[str, np.ndarray], np.ndarray, np.ndarray, np.ndarray, dict[str, Any]]:
        if self.batch is None:
            raise RuntimeError("the batch is closed")
        actions = np.asarray(actions, dtype=np.float64)
        stepping = ~self._ended
        rows = [list(encode_row(self.action_mode, self._slot, actions[k], np.empty(self._width))) if stepping[k] else None for k in range(self.num_envs)]
        merged: list[Any] = [None] * self.num_envs
        if stepping.any():
            stepped = self.runtime.batch_step(self.batch, rows, mask=list(stepping))
            self._rows("step", stepped)
            merged = list(stepped.rows)
        seeds: list[int | None] = [None] * self.num_envs
        if self._ended.any():
            seeds = self._seeds(self._ended)
            restarted = self.runtime.batch_reset(self.batch, seeds, mask=list(self._ended))
            self._rows("reset", restarted)
            merged = [restarted.rows[k] if self._ended[k] else merged[k] for k in range(self.num_envs)]
        obs, infos = self._collect(merged)
        rewards = np.zeros(self.num_envs)
        terminated = np.zeros(self.num_envs, dtype=bool)
        truncated = np.zeros(self.num_envs, dtype=bool)
        for k in np.flatnonzero(stepping):
            header = merged[k].header
            info_k = {key: value[k] for key, value in infos.items() if key != "episode_seed"}
            rewards[k] = float(self.reward_fn(float(header["reward"]), info_k))
            terminated[k], truncated[k] = bool(header["terminated"]), bool(header["truncated"])
        infos["episode_seed"] = np.array(seeds, dtype=object)
        infos["native_reward"] = np.array([float(merged[k].header["reward"]) if stepping[k] else 0.0 for k in range(self.num_envs)])
        self._ended = terminated | truncated
        return obs, rewards, terminated, truncated, infos

    def _collect(self, rows: Sequence[Any]) -> tuple[dict[str, np.ndarray], dict[str, Any]]:
        observations, infos = [], []
        for row in rows:
            if row is None:
                raise ProtocolError("a batch row is missing")
            state, info = unpack_state(row.header, row.tail, self._state, self.info_channel)
            observations.append(state)
            infos.append(info)
        obs = {name: np.stack([o[name] for o in observations]) for name in self._state}
        keys = set().union(*(i.keys() for i in infos))
        info = {key: np.array([i.get(key) for i in infos], dtype=object) for key in keys}
        return obs, info

    def close_extras(self, **kwargs: Any) -> None:
        batch, self.batch = self.batch, None
        if batch is not None and self.runtime.alive:
            self.runtime.batch_close(batch)
