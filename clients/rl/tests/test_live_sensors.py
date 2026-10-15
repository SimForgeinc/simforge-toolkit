"""Camera observations end to end on a GPU: SIMFORGE_BIN, SIMFORGE_RL_TEST_SENSOR_MAP (an installed 3D map pack with a bundled task set)
and SIMFORGE_RL_TEST_ACTOR_CLOSURE (the actor closure the installed runtime ships, under share/actors/closures)."""

from __future__ import annotations

import os
from pathlib import Path

import numpy as np
import pytest

from simforge_rl import EnvServer, ScenarioSet, SimForgeEnv, SocketTransport
from simforge_rl.scenarios import _workspaces

BIN = os.environ.get("SIMFORGE_BIN")
MAP = os.environ.get("SIMFORGE_RL_TEST_SENSOR_MAP")
CLOSURE = os.environ.get("SIMFORGE_RL_TEST_ACTOR_CLOSURE")
if not (BIN and MAP and CLOSURE):
    pytest.skip("set SIMFORGE_BIN, SIMFORGE_RL_TEST_SENSOR_MAP and SIMFORGE_RL_TEST_ACTOR_CLOSURE for the live camera tests", allow_module_level=True)

RIG = Path(__file__).resolve().parents[1] / "simforge_rl/examples/rigs/alpamayo-fisheye.car.rig.json"
CAMERAS = ("camera_front_wide_120fov", "camera_front_tele_30fov")
PRIVILEGED = {"state_vector", "objects", "object_ids", "reward_terms", "events", "minima", "causal", "ego"}


def test_the_fisheye_rig_renders_camera_only_observations(tmp_path: Path) -> None:
    tasks = ScenarioSet.bundled(MAP)
    one = ScenarioSet.of([tasks.scenarios[0]], tasks.map)
    ws = _workspaces(one, tmp_path, None)[one.scenarios[0].name]
    with EnvServer(ws, tmp_path / "s.sock", "--rig", os.fspath(RIG), "--observation-profile", "sensors", "--passes", "rgb,depth",
                   "--actor-closure", CLOSURE, "--max-sessions", "1", binary=BIN) as server:
        env = SimForgeEnv(SocketTransport(server.socket), observation="sensors")
        obs, info = env.reset(options={"episode_seed": 1})
        assert {f"{c}/{p}" for c in CAMERAS for p in ("rgb", "depth")} == set(obs)
        assert env.observation_space.contains(obs)
        frames = [obs["camera_front_wide_120fov/rgb"]]
        for _ in range(10):
            obs, reward, terminated, truncated, info = env.step([10.0, 0.0])
            assert env.observation_space.contains(obs) and np.isfinite(reward)
            assert not PRIVILEGED & set(info), sorted(PRIVILEGED & set(info))
            frames.append(obs["camera_front_wide_120fov/rgb"])
        rgb = frames[-1][..., :3].astype(np.float64)
        assert rgb.std() > 5.0, "a rendered frame, not a blank one"
        assert np.abs(frames[-1].astype(np.int16) - frames[0].astype(np.int16)).mean() > 0.5, "the view moves with the ego"
        depth = obs["camera_front_wide_120fov/depth"]
        assert depth.dtype == np.float32 and np.isfinite(depth).all()
        env.close()
