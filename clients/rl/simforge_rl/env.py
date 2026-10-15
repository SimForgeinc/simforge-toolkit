"""Gymnasium environment over one SimForge episode session."""

from __future__ import annotations

import math
from typing import Any, Literal, Mapping

import gymnasium as gym
import numpy as np
from gymnasium import spaces

from . import wire
from .rewards import RewardFn, native
from .transport import EnvTransport

ActionMode = Literal["setpoint", "control"]
ObservationMode = Literal["state", "sensors", "state+sensors"]
CAMERA_PASSES = {"rgb": 4, "id": 4, "semantic": 4}


def action_space_for(mode: ActionMode) -> spaces.Box:
    if mode == "setpoint":
        return spaces.Box(np.array([0.0, -np.inf]), np.array([np.inf, np.inf]), (2,), np.float64)
    if mode == "control":
        return spaces.Box(np.array([0.0, 0.0, -1.0]), np.array([1.0, 1.0, 1.0]), (3,), np.float64)
    raise ValueError(f"unknown action_mode {mode!r}; expected 'setpoint' or 'control'")


def state_space(hello: Mapping[str, Any]) -> spaces.Dict:
    """Privileged engine state: ego ``state_vector``, ``objects`` rows, optional ``bev``."""
    max_objects, features = int(hello["maxObjects"]), int(hello["objectFeatures"])
    members: dict[str, spaces.Space[Any]] = {}
    if hello.get("stateVectorEnabled", True):
        members["state_vector"] = spaces.Box(-np.inf, np.inf, (int(hello["stateVectorSize"]),), np.float64)
    low = np.tile(np.array([0.0, -math.pi, -np.inf, 0.0, 0.0], dtype=np.float32), (max_objects, 1))
    high = np.tile(np.array([np.inf, math.pi, np.inf, 1.0, 1.0], dtype=np.float32), (max_objects, 1))
    members["objects"] = spaces.Box(low[:, :features], high[:, :features], (max_objects, features), np.float32)
    if hello.get("bevShape"):
        members["bev"] = spaces.Box(-np.inf, np.inf, tuple(hello["bevShape"]), np.float32)
    return spaces.Dict(members)


def sensor_space(hello: Mapping[str, Any]) -> spaces.Dict:
    """Rendered camera passes, keyed ``"<sourceId>/<pass>"``; non-camera payloads stay in ``info``."""
    members: dict[str, spaces.Space[Any]] = {}
    for source in hello.get("sensors", []):
        if source.get("modality") != "rgb":
            continue
        h, w = int(source["height"]), int(source["width"])
        for name in source.get("passes", []):
            if name == "depth":
                members[f"{source['sourceId']}/depth"] = spaces.Box(-np.inf, np.inf, (h, w), np.float32)
            elif name in CAMERA_PASSES:
                members[f"{source['sourceId']}/{name}"] = spaces.Box(0, 255, (h, w, CAMERA_PASSES[name]), np.uint8)
    if not members:
        raise ValueError("observation mode needs rendered cameras: serve with --rig (not --no-sensors)")
    return spaces.Dict(members)


class SimForgeEnv(gym.Env[dict[str, Any], np.ndarray]):
    """Reset/step one SimForge episode; ``terminated``/``truncated`` are the runtime's."""

    metadata: dict[str, Any] = {"render_modes": []}

    def __init__(
        self,
        transport: EnvTransport,
        *,
        action_mode: ActionMode = "setpoint",
        observation: ObservationMode = "state",
        reward_fn: RewardFn = native,
        info_channel: bool = True,
        seed: int | str | None = None,
    ) -> None:
        super().__init__()
        self.transport = transport
        try:
            self._configure(action_mode, observation)
        except BaseException:
            self.close()
            raise
        self.reward_fn = reward_fn
        self.info_channel = info_channel
        self._default_seed = seed

    def _configure(self, action_mode: ActionMode, observation: ObservationMode) -> None:
        self.hello = self.transport.hello()
        self.action_mode: ActionMode = action_mode
        self.observation_mode: ObservationMode = observation
        self._slot = {name: index for index, name in enumerate(self.hello["actionFields"])}
        self._row = np.empty(int(self.hello["actionWidth"]), dtype=np.float64)
        self.ego: str = self.hello["ego"]
        self.decision_hz = int(self.hello["decisionHz"])
        self.action_space = action_space_for(action_mode)
        if observation not in ("state", "sensors", "state+sensors"):
            raise ValueError(f"unknown observation mode {observation!r}")
        self._state_space = dict(state_space(self.hello).spaces) if observation != "sensors" else {}
        self._camera_space = dict(sensor_space(self.hello).spaces) if observation != "state" else {}
        if observation == "state":
            self.observation_space = spaces.Dict(self._state_space)
        elif observation == "sensors":
            self.observation_space = spaces.Dict(self._camera_space)
        else:
            self.observation_space = spaces.Dict({**self._state_space, "sensors": spaces.Dict(self._camera_space)})

    def encode_action(self, action: Any) -> np.ndarray:
        """``action`` in the mode's Box as the native row (NaN = unset)."""
        return encode_row(self.action_mode, self._slot, action, self._row)

    def _unpack(self, header: Mapping[str, Any], tail: bytes) -> tuple[dict[str, Any], dict[str, Any]]:
        if self.observation_mode == "sensors":
            # A sensor policy sees no engine state or labels, whatever profile the runtime serves.
            state, info = {}, {"t_s": header["info"]["t_s"]}
        else:
            state, info = unpack_state(header, tail, self._state_space, self.info_channel)
        frames: dict[str, dict[str, np.ndarray]] = {}
        for source, passes in header.get("sensors", {}).items():
            frames[source] = {name: wire.array(tail, ref) for name, ref in passes.items()}
        info["sensors"] = frames
        if "renderMs" in header:
            info["render_ms"] = header["renderMs"]
        if self.observation_mode == "state":
            return state, info
        cameras: dict[str, np.ndarray] = {}
        for key, space in self._camera_space.items():
            source, name = key.rsplit("/", 1)
            cameras[key] = _declared(key, frames.get(source, {}).get(name), space)
        if self.observation_mode == "sensors":
            return cameras, info
        return {**state, "sensors": cameras}, info

    def reset(self, *, seed: int | None = None, options: Mapping[str, Any] | None = None) -> tuple[dict[str, Any], dict[str, Any]]:
        """``options["episode_seed"]`` overrides the runtime seed; ``options["scenario"]`` switches input."""
        super().reset(seed=seed)
        options = options or {}
        scenario = options.get("scenario")
        if scenario is not None and not isinstance(scenario, Mapping):
            raise TypeError("reset options['scenario'] must be a compiled scenario input mapping")
        episode_seed = options.get("episode_seed", seed if seed is not None else self._default_seed)
        return self._unpack(*self.transport.reset(episode_seed, scenario=scenario))

    def step(self, action: Any) -> tuple[dict[str, Any], float, bool, bool, dict[str, Any]]:
        """Apply ``action`` for one decision; ``None`` keeps the authored control."""
        header, tail = self.transport.step(None if action is None else self.encode_action(action))
        observation, info = self._unpack(header, tail)
        native_reward = float(header["reward"])
        info["native_reward"] = native_reward
        reward = float(self.reward_fn(native_reward, info))
        return observation, reward, bool(header["terminated"]), bool(header["truncated"]), info

    def observe(self) -> tuple[dict[str, Any], dict[str, Any]]:
        return self._unpack(*self.transport.observe())

    def checkpoint(self) -> bytes:
        return self.transport.checkpoint()

    def restore(self, checkpoint: bytes) -> tuple[dict[str, Any], dict[str, Any]]:
        return self._unpack(*self.transport.restore(checkpoint))

    def close(self) -> None:
        transport = self.__dict__.pop("transport", None)
        if transport is not None:
            transport.close()


def _declared(name: str, value: np.ndarray | None, space: spaces.Space[Any]) -> np.ndarray:
    """``value`` if it has the declared shape and dtype; a runtime that breaks the space is a protocol error."""
    if value is None:
        raise wire.ProtocolError(f"the runtime sent no {name} observation")
    if value.shape != space.shape or value.dtype != space.dtype:
        raise wire.ProtocolError(f"{name} is {value.dtype}{list(value.shape)}, but the space declares {space.dtype}{list(space.shape or ())}")
    return value


def unpack_state(header: Mapping[str, Any], tail: bytes, state: Mapping[str, spaces.Space[Any]], info_channel: bool) -> tuple[dict[str, Any], dict[str, Any]]:
    """The declared state arrays and the engine info of one native header."""
    observation = header["observation"]
    values = {name: _declared(name, wire.array(tail, observation[name]) if name in observation else None, space) for name, space in state.items()}
    server_info = dict(header["info"])
    info: dict[str, Any] = {k: server_info[k] for k in ("t_s", "ego", "object_ids", "reward_terms")}
    if info_channel:
        info.update({k: v for k, v in server_info.items() if k not in info})
    return values, info


def encode_row(mode: ActionMode, slots: Mapping[str, int], action: Any, row: np.ndarray) -> np.ndarray:
    """Write ``action`` into the native ``row`` by field slot; every other field is NaN (unset)."""
    values = np.asarray(action, dtype=np.float64).reshape(-1)
    names = ("target_speed_mps", "target_acceleration_mps2") if mode == "setpoint" else ("throttle", "brake", "steer")
    if values.shape != (len(names),):
        raise ValueError(f"{mode} action must have shape ({len(names)},), got {values.shape}")
    if not np.all(np.isfinite(values)):
        raise ValueError(f"action contains non-finite values: {values}")
    row.fill(np.nan)
    for name, value in zip(names, values):
        row[slots[name]] = value
    return row
