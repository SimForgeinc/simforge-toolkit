"""Live runtime tests; need SIMFORGE_BIN and SIMFORGE_RL_TEST_MAP_DIR (richmond-field-station v9, in a directory named by the map id)."""

from __future__ import annotations

import dataclasses
import os
from pathlib import Path

import numpy as np
import pytest

from simforge_rl import EnvServeError, EnvServer, ScenarioServers, ScenarioSet, SimForgeEnv, SocketTransport, WeightedTerms, make_vector_env, template_workspace

BIN = os.environ.get("SIMFORGE_BIN")
MAP_DIR = os.environ.get("SIMFORGE_RL_TEST_MAP_DIR")
if not (BIN and MAP_DIR):
    pytest.skip("set SIMFORGE_BIN and SIMFORGE_RL_TEST_MAP_DIR for the live runtime tests", allow_module_level=True)

LIVE = {"light-corridor@1c0e6bed": "light-corridor", "urban-mixed@42144495": "urban-mixed", "ltap-opposing@9fc1b2c2": "ltap-opposing"}
RICHMOND = ScenarioSet.bundled("richmond-field-station")
TASKS = ScenarioSet.of([dataclasses.replace(s, name=LIVE[s.name]) for s in RICHMOND.scenarios if s.name in LIVE], RICHMOND.map)


@pytest.fixture(scope="module")
def servers():
    with ScenarioServers(TASKS, "--no-sensors", sessions=6, map_dir=MAP_DIR, binary=BIN) as running:
        yield running


def rollout(env: SimForgeEnv, seed: int, speeds: list[float]) -> list[tuple]:
    obs, _ = env.reset(seed=seed)
    trace = [obs["state_vector"].tobytes()]
    for speed in speeds:
        obs, reward, terminated, truncated, info = env.step([speed, 0.0])
        trace.append((obs["state_vector"].tobytes(), obs["objects"].tobytes(), reward, terminated, truncated))
        if terminated or truncated:
            break
    return trace


def test_bundled_scenarios_serve_and_share_spaces(servers: ScenarioServers) -> None:
    envs = {name: SimForgeEnv(SocketTransport(path)) for name, path in servers.sockets.items()}
    spaces = {name: env.observation_space for name, env in envs.items()}
    assert len({repr(s) for s in spaces.values()}) == 1
    for env in envs.values():
        obs, info = env.reset(seed=1)
        assert env.observation_space.contains(obs)
        assert set(info["reward_terms"]) >= {"progress", "comfort", "proximity"}
        env.close()


def test_same_seed_and_actions_replay_identically(servers: ScenarioServers) -> None:
    a = SimForgeEnv(SocketTransport(servers.sockets["ltap-opposing"]))
    b = SimForgeEnv(SocketTransport(servers.sockets["ltap-opposing"]))
    speeds = [8.0, 12.0, 16.0, 20.0] * 10
    assert rollout(a, 5, speeds) == rollout(b, 5, speeds)


def test_checkpoint_restore_reproduces_the_continuation(servers: ScenarioServers) -> None:
    env = SimForgeEnv(SocketTransport(servers.sockets["urban-mixed"]))
    env.reset(seed=2)
    for _ in range(10):
        env.step([6.0, 0.0])
    snap = env.checkpoint()
    first = [env.step([9.0, 0.5])[1] for _ in range(10)]
    env.restore(snap)
    assert [env.step([9.0, 0.5])[1] for _ in range(10)] == first


def test_reward_hook_matches_runtime_terms(servers: ScenarioServers) -> None:
    hook = WeightedTerms({"progress": 1.0, "comfort": 1.0, "proximity": 1.0})
    env = SimForgeEnv(SocketTransport(servers.sockets["light-corridor"]), reward_fn=hook)
    env.reset(seed=3)
    for _ in range(20):
        _, reward, _, _, info = env.step([15.0, 0.0])
        assert reward == pytest.approx(info["native_reward"])


def vector_run(servers: ScenarioServers) -> tuple[list, list]:
    envs = make_vector_env(TASKS, servers.socket_sets(2), 2, vectorization="sync")
    _, info = envs.reset(seed=11)
    draws = [tuple(zip(info["scenario"], info["episode_seed"]))]
    rewards, transitions = [], []
    for _ in range(170):
        obs, reward, terminated, truncated, info = envs.step(np.array([[10.0, 0.0], [12.0, 0.0]]))
        rewards.append(reward.tolist())
        for i in np.flatnonzero(terminated | truncated):
            final = info["final_obs"][i]["state_vector"]
            assert not np.array_equal(final, obs["state_vector"][i])
            transitions.append((i, info["scenario"][i], int(info["episode_seed"][i])))
    envs.close()
    return rewards, draws + transitions


def test_vector_env_replays_and_autoresets_with_new_draws(servers: ScenarioServers) -> None:
    first, second = vector_run(servers), vector_run(servers)
    assert first == second
    rewards, draws = first
    initial, transitions = draws[0], draws[1:]
    assert {i for i, _, _ in transitions} == {0, 1}
    for i, scenario, seed in transitions:
        assert (scenario, seed) != initial[i]


def test_runtime_refusals_carry_stable_codes(tmp_path: Path) -> None:
    corridor = TASKS.scenarios[TASKS.index("light-corridor")]
    ws = template_workspace(corridor.template, MAP_DIR, tmp_path / "ws", site=corridor.site)
    with EnvServer(ws, tmp_path / "e.sock", "--no-sensors", "--map-dir", MAP_DIR, binary=BIN) as server:
        transport = SocketTransport(server.socket)
        with pytest.raises(EnvServeError) as early:
            transport.step([12.0, 0.0])
        assert early.value.code == "episode_not_reset"
        transport.reset(1)
        with pytest.raises(EnvServeError) as narrow:
            transport.step([1.0])
        assert narrow.value.code == "episode_error"
    with pytest.raises(EnvServeError) as refused:
        EnvServer(tmp_path / "missing", tmp_path / "m.sock", "--no-sensors", binary=BIN)
    assert refused.value.code == "workspace_not_found"
