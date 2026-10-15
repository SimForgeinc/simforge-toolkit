from __future__ import annotations

import numpy as np
import pytest

from simforge_rl import EnvServeError, ProtocolError, RuntimeClient, SimForgeBatchVectorEnv

from .fakes import FakeRuntime


def runtime(**kwargs) -> RuntimeClient:
    fake = FakeRuntime(**kwargs)
    return RuntimeClient(fake.client_reader, fake.client_writer)


def test_ended_envs_reset_on_the_next_step_only() -> None:
    envs = SimForgeBatchVectorEnv(runtime(), ["ws"], 2)
    obs, info = envs.reset(seed=3)
    first_seeds = list(info["episode_seed"])
    assert envs.observation_space.contains(obs)
    for _ in range(2):
        obs, reward, terminated, truncated, _ = envs.step(np.array([[4.0, 0.0], [6.0, 0.0]]))
        assert not (terminated | truncated).any()
    obs, reward, terminated, truncated, _ = envs.step(np.array([[4.0, 0.0], [25.0, 0.0]]))
    assert truncated[0] and terminated[1]
    obs, reward, terminated, truncated, info = envs.step(np.array([[9.0, 0.0], [9.0, 0.0]]))
    assert (reward == 0).all() and not (terminated | truncated).any()
    assert (obs["state_vector"][:, 0] == 0).all()
    assert [int(s) for s in info["episode_seed"]] != first_seeds
    obs, *_ = envs.step(np.array([[9.0, 0.0], [9.0, 0.0]]))
    assert (obs["state_vector"][:, 1] == 9.0).all()
    envs.close()


def test_seeded_batches_replay() -> None:
    def trace() -> list:
        envs = SimForgeBatchVectorEnv(runtime(), ["ws"], 3)
        out = [envs.reset(seed=8)[1]["episode_seed"].tolist()]
        for k in range(8):
            obs, reward, *_ , info = envs.step(np.full((3, 2), [5.0 + k, 0.0]))
            out.append((obs["state_vector"].tolist(), reward.tolist(), info["episode_seed"].tolist()))
        return out

    assert trace() == trace()


def test_failed_rows_keep_their_runtime_headers() -> None:
    rt = runtime()
    batch = rt.batch_create(["ws"], 3)
    rt.batch_reset(batch, mask=[True, False, True])
    results = rt.batch_step(batch, [[1.0] * 6, [1.0] * 6, [1.0] * 6])
    assert results.failed == (1,)
    assert results.rows[1].header["code"] == "episode_not_reset" and not results.rows[1].ok
    assert results.rows[0].ok and results.rows[2].ok
    masked = rt.batch_step(batch, [[1.0] * 6] * 3, mask=[True, False, False])
    assert masked.rows[1] is None and masked.rows[2] is None and masked.failed == ()


def test_missing_selected_row_is_a_protocol_error() -> None:
    def drop_row(request):
        if request["op"] == "env.batch.step":
            return {"ok": True, "result": {"batchId": request["params"]["batchId"], "failed": [], "results": [None, None]}}
        return None

    envs = SimForgeBatchVectorEnv(runtime(override=drop_row), ["ws"], 2)
    envs.reset(seed=0)
    with pytest.raises(ProtocolError):
        envs.step(np.zeros((2, 2)))


def test_failed_list_must_match_rows() -> None:
    def lie(request):
        if request["op"] == "env.batch.observe":
            return {"ok": True, "result": {"batchId": request["params"]["batchId"], "failed": [], "results": [
                {"envIndex": 0, "ok": False, "header": {"ok": False, "code": "episode_error", "reason": "x"}, "tailBase64": ""}]}}
        return None

    rt = runtime(override=lie)
    batch = rt.batch_create(["ws"], 1)
    with pytest.raises(ProtocolError, match="disagrees"):
        rt.batch_observe(batch)


def test_gym_error_exposes_failed_codes() -> None:
    envs = SimForgeBatchVectorEnv(runtime(), ["ws"], 2)
    with pytest.raises(EnvServeError) as error:
        envs.step(np.zeros((2, 2)))
    assert error.value.detail == {0: "episode_not_reset", 1: "episode_not_reset"}
    assert error.value.results.failed == (0, 1)


@pytest.mark.parametrize("count, shards", [(2.5, None), (True, None), (0, None), (65, None), (2, 3), (2, 1.0)])
def test_batch_size_is_refused_before_anything_is_allocated(count, shards) -> None:
    fake = FakeRuntime()
    rt = RuntimeClient(fake.client_reader, fake.client_writer)
    with pytest.raises((TypeError, ValueError)):
        rt.batch_create(["ws"], count, shards=shards)
    assert fake.batches == {}


def test_non_boolean_mask_is_refused_before_any_reset() -> None:
    fake = FakeRuntime()
    rt = RuntimeClient(fake.client_reader, fake.client_writer)
    batch = rt.batch_create(["ws"], 2)
    with pytest.raises(TypeError):
        rt.batch_reset(batch, mask=["false", True])
    assert fake.batches[batch.batch_id]["live"] == [False, False]


@pytest.mark.parametrize("row_ok, header_ok", [(None, False), (False, True), (True, None), ("false", False)])
def test_row_ok_must_be_a_boolean_agreeing_with_its_header(row_ok, header_ok) -> None:
    def answer(request):
        if request["op"] == "env.batch.observe":
            return {"ok": True, "result": {"batchId": request["params"]["batchId"], "failed": [0], "results": [
                {"envIndex": 0, "ok": row_ok, "header": {"ok": header_ok, "code": "episode_error", "reason": "x"}, "tailBase64": ""}]}}
        return None

    rt = runtime(override=answer)
    with pytest.raises(ProtocolError):
        rt.batch_observe(rt.batch_create(["ws"], 1))


def test_unseeded_resets_continue_the_seeded_stream() -> None:
    def draws() -> list:
        envs = SimForgeBatchVectorEnv(runtime(), ["ws"], 2)
        return [envs.reset(seed=8)[1]["episode_seed"].tolist(), envs.reset()[1]["episode_seed"].tolist(), envs.reset()[1]["episode_seed"].tolist()]

    first, second = draws(), draws()
    assert first == second
    assert first[0] != first[1] != first[2]


def test_reset_options_are_refused_without_touching_envs() -> None:
    fake = FakeRuntime()
    envs = SimForgeBatchVectorEnv(RuntimeClient(fake.client_reader, fake.client_writer), ["ws"], 2)
    with pytest.raises(ValueError, match="reset_mask"):
        envs.reset(seed=1, options={"reset_mask": np.array([True, False])})
    assert not any(r["op"] == "env.batch.reset" for r in fake.requests)
