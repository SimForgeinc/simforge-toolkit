from __future__ import annotations

import sys
from pathlib import Path

import pytest

from simforge_rl import ProtocolError, RuntimeClient, RuntimeProtocolError

from .fakes import FakeRuntime


def client(fake: FakeRuntime, **kwargs) -> RuntimeClient:
    return RuntimeClient(fake.client_reader, fake.client_writer, **kwargs)


def test_handshake_refuses_other_protocols_and_contracts() -> None:
    with pytest.raises(ProtocolError):
        client(FakeRuntime(handshake={"runtimeProtocol": 2, "rendererProtocol": 5, "operations": ["handshake"]}))
    with pytest.raises(ProtocolError):
        client(FakeRuntime(handshake={"runtimeProtocol": 1, "rendererProtocol": 4, "operations": ["handshake"]}))
    with pytest.raises(ProtocolError):
        client(FakeRuntime(), expected_contract_sha256="1" * 64)


def test_unlisted_operation_is_refused_without_a_request() -> None:
    fake = FakeRuntime()
    runtime = client(fake)
    with pytest.raises(RuntimeProtocolError) as error:
        runtime.render(["ws"], "2d")
    assert error.value.code == "operation_unavailable"
    assert [r["op"] for r in fake.requests] == ["handshake"]


def test_error_envelope_keeps_code_exit_and_path() -> None:
    def refuse(request):
        if request["op"] == "command":
            return {"ok": False, "error": {"code": "no_site", "reason": "nothing fits", "path": "document.json", "detail": {"n": 1}, "exitCode": 2}}
        return None

    runtime = client(FakeRuntime(override=refuse))
    with pytest.raises(RuntimeProtocolError) as error:
        runtime.command(["instantiate"])
    assert (error.value.code, error.value.exit_code, error.value.path, error.value.detail) == ("no_site", 2, "document.json", {"n": 1})


@pytest.mark.parametrize(
    "reply",
    [
        {"ok": True, "id": "other", "result": {"value": 1, "exitCode": 0}},
        {"ok": True, "op": "help", "result": {"value": 1, "exitCode": 0}},
        {"ok": True, "result": {"value": 1, "exitCode": 1}},
        {"ok": True, "result": "text"},
        {"ok": False},
    ],
)
def test_malformed_success_is_a_protocol_error(reply: dict) -> None:
    runtime = client(FakeRuntime(override=lambda r: reply if r["op"] == "command" else None))
    with pytest.raises(ProtocolError):
        runtime.command(["x"])


def test_invalid_tail_base64_is_refused() -> None:
    def corrupt(request):
        if request["op"] == "env.create":
            return {"ok": True, "result": {"envId": "e", "header": {"ok": True, "protocol": "simforge.env-serve/v1"}, "tailBase64": "@@@="}}
        return None

    runtime = client(FakeRuntime(override=corrupt))
    with pytest.raises(ProtocolError):
        runtime.env_create(["ws"])


def test_spawned_runtime_does_not_inherit_cloud_credentials(monkeypatch: pytest.MonkeyPatch) -> None:
    probe = str(Path(__file__).with_name("env_probe_runtime.py"))
    monkeypatch.setenv("SIMFORGE_API_KEY", "sfk_sentinel")
    monkeypatch.setenv("SIMFORGE_MAPS_REGISTRY_TOKEN", "sentinel")
    monkeypatch.setenv("SIMFORGE_MAPS_CACHE_ROOT", "/maps")
    with RuntimeClient.spawn(sys.executable, probe) as runtime:
        names = runtime.command(["env"]).value
    assert "SIMFORGE_MAPS_CACHE_ROOT" in names
    assert "SIMFORGE_API_KEY" not in names and "SIMFORGE_MAPS_REGISTRY_TOKEN" not in names
    assert runtime.close() == 0
