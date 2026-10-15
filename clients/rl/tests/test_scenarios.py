from __future__ import annotations

import json
from collections import Counter
from pathlib import Path

import numpy as np
import pytest

from simforge_rl import Scenario, ScenarioSet, ScenarioSetEnv, make_vector_env

from .fakes import HELLO, FakeEnvServer


def test_sampling_is_seeded_and_weighted() -> None:
    tasks = ScenarioSet.of([Scenario("a", workspace="wa", weight=3.0), Scenario("b", workspace="wb", weight=1.0, seeds=(7, 8))])
    draws = [tasks.sample(np.random.default_rng(11)) for _ in range(2)]
    assert draws[0] == draws[1]
    rng = np.random.default_rng(0)
    sample = [tasks.sample(rng) for _ in range(4000)]
    counts = Counter(s.name for s, _ in sample)
    assert 0.72 < counts["a"] / 4000 < 0.78
    assert {seed for s, seed in sample if s.name == "b"} == {7, 8}


@pytest.mark.parametrize("weights", [(0.0, 0.0), (-1.0, 2.0), (float("nan"), 1.0)])
def test_invalid_weights_are_refused(weights: tuple[float, float]) -> None:
    with pytest.raises(ValueError):
        ScenarioSet.of([Scenario("a", workspace="x", weight=weights[0]), Scenario("b", workspace="y", weight=weights[1])])


def test_scenario_needs_exactly_one_source_and_unique_names() -> None:
    with pytest.raises(ValueError):
        Scenario("a")
    with pytest.raises(ValueError):
        Scenario("a", workspace="w", endpoint="e")
    with pytest.raises(ValueError):
        ScenarioSet.of([Scenario("a", workspace="w"), Scenario("a", workspace="v")])


def test_save_load_resolves_paths_relative_to_the_file(tmp_path: Path) -> None:
    (tmp_path / "sets").mkdir()
    path = tmp_path / "sets" / "tasks.json"
    path.write_text(json.dumps({"schema": "simforge-rl.scenario-set/v1", "scenarios": [
        {"name": "a", "template": "../t/a.json", "weight": 2, "seeds": [1]}, {"name": "b", "endpoint": "/run/b.sock"}]}))
    tasks = ScenarioSet.load(path)
    assert tasks.scenarios[0].template == str((tmp_path / "t" / "a.json").resolve())
    assert tasks.scenarios[1].endpoint == "/run/b.sock"
    tasks.save(tmp_path / "copy.json")
    assert ScenarioSet.load(tmp_path / "copy.json") == tasks
    path.write_text(json.dumps({"schema": "simforge-rl.scenario-set/v1", "scenarios": [{"name": "a", "workspace": "w", "wieght": 2}]}))
    with pytest.raises(ValueError, match="wieght"):
        ScenarioSet.load(path)


def test_scenario_set_env_draws_pins_and_rejects_mixed_spaces(tmp_path: Path) -> None:
    servers = {n: FakeEnvServer(tmp_path / f"{n}.sock") for n in ("a", "b")}
    tasks = ScenarioSet.of([Scenario(n, endpoint=s.path) for n, s in servers.items()])
    env = ScenarioSetEnv(tasks)
    seen = {env.reset(seed=s)[1]["scenario"] for s in range(20)}
    assert seen == {"a", "b"}
    obs, info = env.reset(seed=1, options={"scenario_name": "b", "episode_seed": 33})
    assert (info["scenario"], info["episode_seed"], obs["state_vector"][2]) == ("b", 33, 33.0)
    first = [env.reset(seed=4)[1]["episode_seed"] for _ in range(2)]
    assert first[0] == first[1]
    odd = FakeEnvServer(tmp_path / "odd.sock", hello={**HELLO, "maxObjects": 3})
    with pytest.raises(ValueError, match="different spaces"):
        ScenarioSetEnv(ScenarioSet.of([Scenario("a", endpoint=servers["a"].path), Scenario("odd", endpoint=odd.path)]))


def test_max_open_opens_sessions_on_draw_and_closes_the_least_recent(tmp_path: Path) -> None:
    servers = {n: FakeEnvServer(tmp_path / f"{n}.sock") for n in ("a", "b", "c")}
    tasks = ScenarioSet.of([Scenario(n, endpoint=s.path) for n, s in servers.items()])
    env = ScenarioSetEnv(tasks, max_open=2)
    assert list(env.envs) == ["a"]
    for name in ("b", "c", "a"):
        assert env.reset(options={"scenario_name": name})[1]["scenario"] == name
        assert len(env.envs) <= 2 and list(env.envs)[-1] == name
    assert list(env.envs) == ["c", "a"]
    env.close()
    odd = FakeEnvServer(tmp_path / "odd.sock", hello={**HELLO, "maxObjects": 3})
    lazy = ScenarioSetEnv(ScenarioSet.of([Scenario("a", endpoint=servers["a"].path), Scenario("odd", endpoint=odd.path)]), max_open=2)
    with pytest.raises(ValueError, match="different spaces"):
        lazy.reset(options={"scenario_name": "odd"})
    lazy.close()


@pytest.mark.parametrize("vectorization", ["sync", "async"])
def test_vector_env_autoresets_in_the_same_step(tmp_path: Path, vectorization: str) -> None:
    server = FakeEnvServer(tmp_path / "v.sock")
    tasks = ScenarioSet.of([Scenario("a", endpoint=server.path)])
    envs = make_vector_env(tasks, None, 2, vectorization=vectorization)
    obs, info = envs.reset(seed=0)
    assert obs["state_vector"].shape == (2, 3)
    assert info["episode_seed"][0] != info["episode_seed"][1]
    for _ in range(2):
        envs.step(np.array([[1.0, 0.0], [2.0, 0.0]]))
    obs, reward, terminated, truncated, info = envs.step(np.array([[1.0, 0.0], [2.0, 0.0]]))
    assert truncated.all()
    assert (obs["state_vector"][:, 0] == 0).all()
    assert (info["final_obs"][0]["state_vector"][0], info["final_obs"][1]["state_vector"][1]) == (3.0, 2.0)
    envs.close()
