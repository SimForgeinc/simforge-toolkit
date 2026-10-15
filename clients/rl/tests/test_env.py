from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from simforge_rl import EnvServeError, EventPenalty, ProtocolError, SimForgeEnv, SocketTransport, WeightedTerms

from .fakes import CAMERA, FIELDS, HELLO, FakeEnvServer


@pytest.fixture()
def server(tmp_path: Path):
    fake = FakeEnvServer(tmp_path / "env.sock")
    yield fake
    fake.close()


def test_setpoint_fills_only_its_slots_and_none_delegates(server: FakeEnvServer) -> None:
    env = SimForgeEnv(SocketTransport(server.path))
    obs, info = env.reset(seed=5)
    assert env.observation_space.contains(obs)
    env.step([12.0, -1.0])
    env.step(None)
    sent = server.episodes[0].actions
    assert sent[0][:2] == [12.0, -1.0] and all(v is None for v in sent[0][2:])
    assert sent[1] is None
    env.close()


def test_control_mode_slots_and_shape_errors(server: FakeEnvServer) -> None:
    env = SimForgeEnv(SocketTransport(server.path), action_mode="control")
    env.reset(seed=1)
    env.step([0.2, 0.0, -0.3])
    row = server.episodes[0].actions[0]
    assert [row[FIELDS.index(k)] for k in ("throttle", "brake", "steer")] == [0.2, 0.0, -0.3]
    with pytest.raises(ValueError):
        env.step([0.2, 0.0])
    with pytest.raises(ValueError):
        env.step([np.nan, 0.0, 0.0])
    env.close()


def test_reward_hook_replaces_reward_and_keeps_native(server: FakeEnvServer) -> None:
    env = SimForgeEnv(SocketTransport(server.path), reward_fn=WeightedTerms({"progress": 2.0}, bias=1.0))
    env.reset(seed=0)
    _, reward, _, _, info = env.step([10.0, 0.0])
    assert reward == pytest.approx(1.0 + 2.0 * 1.0)
    assert info["native_reward"] == pytest.approx(0.5)
    strict = SimForgeEnv(SocketTransport(server.path), reward_fn=WeightedTerms({"absent": 1.0}))
    strict.reset(seed=0)
    with pytest.raises(KeyError):
        strict.step([10.0, 0.0])


def test_event_penalty_and_runtime_termination(server: FakeEnvServer) -> None:
    env = SimForgeEnv(SocketTransport(server.path), reward_fn=EventPenalty({"collision": 10.0}))
    env.reset(seed=0)
    _, reward, terminated, truncated, info = env.step([25.0, 0.0])
    assert terminated and not truncated
    assert reward == pytest.approx(25.0 / 10 - 0.5 - 10.0)
    no_channel = SimForgeEnv(SocketTransport(server.path), reward_fn=EventPenalty({"collision": 10.0}), info_channel=False)
    no_channel.reset(seed=0)
    with pytest.raises(KeyError):
        no_channel.step([25.0, 0.0])


def test_episode_seed_option_and_checkpoint_restore(server: FakeEnvServer) -> None:
    env = SimForgeEnv(SocketTransport(server.path))
    obs, _ = env.reset(seed=3, options={"episode_seed": 42})
    assert obs["state_vector"][2] == 42
    env.step([4.0, 0.0])
    snap = env.checkpoint()
    after, *_ = env.step([8.0, 0.0])
    restored, _ = env.restore(snap)
    assert restored["state_vector"][1] == 4.0
    again, *_ = env.step([8.0, 0.0])
    np.testing.assert_array_equal(again["state_vector"], after["state_vector"])


def test_sensor_observation_modes(tmp_path: Path) -> None:
    fake = FakeEnvServer(tmp_path / "s.sock", hello={**HELLO, "sensors": [CAMERA, {"sourceId": "lidar", "modality": "lidar"}]})
    env = SimForgeEnv(SocketTransport(fake.path), observation="sensors")
    obs, info = env.reset(seed=0)
    assert set(obs) == {"front/rgb", "front/depth"}
    assert env.observation_space.contains(obs)
    obs, *_ = env.step([1.0, 0.0])
    assert obs["front/rgb"].shape == (2, 4, 4) and obs["front/rgb"][0, 0, 0] == 1
    both = SimForgeEnv(SocketTransport(fake.path), observation="state+sensors")
    obs, _ = both.reset(seed=0)
    assert both.observation_space.contains(obs) and "state_vector" in obs and "front/depth" in obs["sensors"]


def test_sensor_mode_without_cameras_is_refused(server: FakeEnvServer) -> None:
    with pytest.raises(ValueError, match="--rig"):
        SimForgeEnv(SocketTransport(server.path), observation="sensors")


def test_refusal_carries_stable_code(server: FakeEnvServer) -> None:
    transport = SocketTransport(server.path)
    transport.hello()
    with pytest.raises(EnvServeError) as error:
        transport.call("bogus")
    assert error.value.code == "unknown_op"


def test_wrong_protocol_and_unanswered_request_are_protocol_errors(tmp_path: Path) -> None:
    other = FakeEnvServer(tmp_path / "a.sock", hello={**HELLO, "protocol": "simforge.env-serve/v2"})
    with pytest.raises(ProtocolError):
        SimForgeEnv(SocketTransport(other.path))
    skewed = FakeEnvServer(tmp_path / "b.sock", mutate=lambda h, t: ({**h, "i": h["i"] + 1}, t))
    with pytest.raises(ProtocolError):
        SocketTransport(skewed.path).hello()
    truncated = FakeEnvServer(tmp_path / "c.sock", mutate=lambda h, t: (h, t[:-4]) if "observation" in h else (h, t))
    env = SimForgeEnv(SocketTransport(truncated.path))
    with pytest.raises(ProtocolError):
        env.reset(seed=0)


def test_disabled_state_vector_is_dropped_to_match_the_space(tmp_path: Path) -> None:
    fake = FakeEnvServer(tmp_path / "d.sock", hello={**HELLO, "stateVectorEnabled": False})
    env = SimForgeEnv(SocketTransport(fake.path))
    obs, _ = env.reset(seed=0)
    assert set(obs) == {"objects"} and env.observation_space.contains(obs)
    obs, *_ = env.step([1.0, 0.0])
    assert env.observation_space.contains(obs)


def test_frames_that_break_the_declared_camera_space_are_refused(tmp_path: Path) -> None:
    fake = FakeEnvServer(tmp_path / "w.sock", hello={**HELLO, "sensors": [{**CAMERA, "width": 8}]})
    env = SimForgeEnv(SocketTransport(fake.path), observation="sensors")
    with pytest.raises(ProtocolError, match="space declares"):
        env.reset(seed=0)
    missing = FakeEnvServer(tmp_path / "m.sock", hello={**HELLO, "sensors": [{**CAMERA, "passes": ["rgb", "semantic"]}]})
    with pytest.raises(ProtocolError, match="semantic"):
        SimForgeEnv(SocketTransport(missing.path), observation="sensors").reset(seed=0)


def test_sensor_policy_sees_no_engine_state_or_labels(tmp_path: Path) -> None:
    fake = FakeEnvServer(tmp_path / "p.sock", hello={**HELLO, "sensors": [CAMERA]})
    env = SimForgeEnv(SocketTransport(fake.path), observation="sensors")
    obs, info = env.reset(seed=0)
    assert set(info) == {"t_s", "sensors"}
    obs, reward, terminated, truncated, info = env.step([25.0, 0.0])
    assert set(info) == {"t_s", "sensors", "native_reward"} and terminated
    profile_only = {**HELLO, "sensors": [CAMERA], "stateVectorEnabled": False, "stateVectorSize": 0, "maxObjects": 0, "objectFeatures": 0}
    sensors_profile = FakeEnvServer(tmp_path / "q.sock", hello=profile_only,
                                    mutate=lambda h, t: ({**h, "info": {"t_s": h["info"]["t_s"]}} if "info" in h else h, t))
    obs, info = SimForgeEnv(SocketTransport(sensors_profile.path), observation="sensors").reset(seed=0)
    assert set(obs) == {"front/rgb", "front/depth"} and set(info) == {"t_s", "sensors"}
