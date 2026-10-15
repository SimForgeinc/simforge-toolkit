from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

import httpx
import pytest

from simforge_cloud import ArtifactIntegrityError, SimforgeApiError, SimforgeClient


RUN = {
    "id": "run_01J00000000000000000000000",
    "type": "noop",
    "status": "succeeded",
    "input": {},
    "createdAt": "2026-01-01T00:00:00Z",
    "startedAt": None,
    "finishedAt": "2026-01-01T00:00:01Z",
    "createdBy": "user_1",
    "costEstimateCents": None,
    "costCents": None,
    "providerCostCents": None,
    "providerCostEvidence": None,
    "billing": {"hold": None, "heldCents": 0, "chargedCents": 0},
    "jobs": [],
    "warnings": [],
    "result": {"ok": True},
}


def test_auth_and_automatic_idempotency_header() -> None:
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        return httpx.Response(201, json=RUN)

    transport = httpx.MockTransport(handler)
    with httpx.Client(transport=transport, base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            result = client.runs.create(type="noop", input={})
            client.runs.create(type="noop", input={}, idempotency_key="caller-key")
    assert result.id == RUN["id"]
    assert seen[0].headers["Authorization"] == "Bearer sfk_test"
    assert seen[0].headers["Idempotency-Key"]
    assert seen[1].headers["Idempotency-Key"] == "caller-key"


def test_pagination_iteration() -> None:
    calls: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        calls.append(str(request.url))
        if request.url.params.get("cursor") == "next":
            return httpx.Response(200, json={"data": [{"id": "sc_2", "name": "Two", "mapId": None, "mapVersionId": None, "latestRevisionId": None, "updatedAt": "2026-01-01", "createdBy": None, "tags": [], "datasetIds": []}], "nextCursor": None})
        return httpx.Response(200, json={"data": [{"id": "sc_1", "name": "One", "mapId": None, "mapVersionId": None, "latestRevisionId": None, "updatedAt": "2026-01-01", "createdBy": None, "tags": [], "datasetIds": []}], "nextCursor": "next"})

    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            scenarios = list(client.scenarios.list(limit=1))
    assert [scenario.id for scenario in scenarios] == ["sc_1", "sc_2"]
    assert len(calls) == 2
    assert "cursor=next" in calls[1]


def test_error_mapping_preserves_envelope_fields() -> None:
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            403,
            headers={"X-Request-Id": "req_header"},
            json={"error": {"code": "insufficient_scope", "reason": "Need maps:read.", "detail": {"required": ["maps:read"]}, "requestId": "req_body"}},
        )

    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            with pytest.raises(SimforgeApiError) as raised:
                list(client.maps.list())
    error = raised.value
    assert (error.code, error.reason, error.detail, error.request_id) == (
        "insufficient_scope",
        "Need maps:read.",
        {"required": ["maps:read"]},
        "req_body",
    )


def test_wait_polls_events_and_returns_terminal_run() -> None:
    state = {"polls": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path.endswith("/events"):
            state["polls"] += 1
            return httpx.Response(200, json={"data": [], "nextAfter": state["polls"]})
        result = dict(RUN)
        result["status"] = "queued" if state["polls"] == 1 else "succeeded"
        return httpx.Response(200, json=result)

    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            result = client.runs.wait(RUN["id"], timeout=1, poll_interval=0)
    assert result.status == "succeeded"
    assert state["polls"] == 2


def test_download_hash_mismatch_removes_partial_file(tmp_path: Path) -> None:
    content = b"not the expected artifact"
    expected = hashlib.sha256(b"different").hexdigest()
    partial_requests: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        partial_requests.append(request.url.path)
        if request.url.path.endswith("/download"):
            return httpx.Response(200, json={"url": "/blob", "expiresAt": "2026-01-01T00:10:00Z", "sha256": expected, "sizeBytes": len(content), "mediaType": "application/octet-stream"})
        return httpx.Response(200, content=content)

    destination = tmp_path / "artifact.bin"
    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            with pytest.raises(ArtifactIntegrityError):
                client.artifacts.download("art_01J00000000000000000000000", destination)
    assert not destination.exists()
    assert list(tmp_path.glob("*.part")) == []
    assert partial_requests == ["/api/v1/artifacts/art_01J00000000000000000000000/download", "/blob"]

def test_workspace_selection_and_access_models() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["X-SimForge-Workspace-Id"] == "ws_test"
        if request.url.path.endswith("/me"):
            return httpx.Response(
                200,
                json={
                    "user": {"id": "user_1", "email": "user@example.test", "name": "User"},
                    "workspace": {"id": "ws_test", "name": "Test", "role": "owner"},
                    "principal": {"kind": "api_key", "scopes": ["maps:read"]},
                },
            )
        return httpx.Response(200, json={"data": [{"id": "ws_test", "name": "Test", "role": "owner"}]})

    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", workspace_id="ws_test", http_client=http) as client:
            me = client.me.get()
            workspaces = client.workspaces.list()
    assert me.workspace.id == "ws_test"
    assert workspaces.data[0].id == "ws_test"

def test_usage_get() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/v1/usage"
        assert request.url.params["from"] == "2026-01-01"
        assert request.url.params["to"] == "2026-01-02"
        return httpx.Response(
            200,
            json={
                "data": [{"day": "2026-01-01", "runs": 1, "costCents": 5, "byType": {"noop": {"runs": 1, "costCents": 5}}}],
                "totalCents": 5,
                "balanceCents": 95,
                "dailyCapCents": None,
            },
        )

    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            usage = client.usage.get(from_="2026-01-01", to="2026-01-02")
    assert usage.total_cents == 5
    assert usage.data[0].by_type["noop"].cost_cents == 5


def test_vehicles_list_pages_shared_then_workspace_models() -> None:
    sedan = {"catalogId": "vehicle.sedan", "name": "Sedan", "source": "shared", "vehicleClass": "car", "dims": {"length": 4.6, "width": 1.9, "height": 1.5}}
    van = {
        "catalogId": "workspace.delivery-van", "name": "Delivery van", "source": "workspace", "vehicleClass": "van",
        "dims": {"length": 5.9, "width": 2.0, "height": 2.6}, "license": "CC-BY-4.0", "attribution": "QA", "glbSha256": "a" * 64,
        "wheels": True, "tintable": False, "createdAt": "2026-10-06T00:00:00.000Z",
    }

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/v1/vehicles"
        if request.url.params.get("cursor") == "next":
            return httpx.Response(200, json={"data": [van], "nextCursor": None})
        return httpx.Response(200, json={"data": [sedan], "nextCursor": "next"})

    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            vehicles = list(client.vehicles.list(limit=1))
    assert [(v.catalog_id, v.source) for v in vehicles] == [("vehicle.sedan", "shared"), ("workspace.delivery-van", "workspace")]
    assert vehicles[1].glb_sha256 == "a" * 64


def test_vehicles_create_and_delete_send_the_api_shapes() -> None:
    seen: list[httpx.Request] = []
    van = {"catalogId": "workspace.delivery-van", "name": "Delivery van", "source": "workspace", "vehicleClass": "van", "dims": {"length": 5.9, "width": 2.0, "height": 2.6}}

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        return httpx.Response(201, json=van) if request.method == "POST" else httpx.Response(204)

    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            created = client.vehicles.create(id="delivery-van", upload_id="upl_1", vehicle_class="van", license="CC-BY-4.0", attribution="QA", dims=(5.9, 2.0, 2.6))
            client.vehicles.delete("workspace.delivery-van")
    assert created.catalog_id == "workspace.delivery-van"
    assert json.loads(seen[0].content) == {"id": "delivery-van", "uploadId": "upl_1", "vehicleClass": "van", "license": "CC-BY-4.0", "attribution": "QA", "dims": [5.9, 2.0, 2.6]}
    assert (seen[1].method, seen[1].url.path) == ("DELETE", "/api/v1/vehicles/workspace.delivery-van")



EXPORT = {
    "id": "exp_1", "dataset": {"id": "ds_1", "name": "Set"}, "scope": {"kind": "dataset", "datasetId": "ds_1"}, "format": "zip",
    "status": "running", "stage": "packaging",
    "progress": {"scenarios": {"done": 0, "failed": 0, "pending": 1, "total": 1}, "bytes": {"done": 0, "total": 10},
                 "startedAt": None, "throughputBytesPerSecond": None, "etaSeconds": None, "readyAt": None},
    "excludedAtPrepare": 0, "failures": {"count": 0, "timedOut": 0, "items": []},
    "createdAt": "2026-10-08T00:00:00Z", "createdBy": "user_1", "finishedAt": None,
}
ESTIMATE = {
    "dryRun": True, "bytes": 10, "withoutRenders": [], "etaSeconds": None, "costEstimateCents": 10,
    "aggregates": {"scenarios": 1, "excluded": 0, "packageBytes": 10, "renders": [], "recordings": {"count": 0, "bytes": 0}, "throughputBytesPerSecond": None},
}


def test_exports_create_distinguishes_estimate_and_pages_files() -> None:
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        if request.url.path == "/api/v1/exports":
            body = json.loads(request.content)
            return httpx.Response(200, json=ESTIMATE) if body.get("dryRun") else httpx.Response(201, json=EXPORT)
        cursor = request.url.params.get("cursor")
        files = [{"path": f"f{cursor or 0}", "sha256": "a" * 64, "size": 1, "url": "https://x"}]
        return httpx.Response(200, json={"files": files, "nextCursor": None if cursor else "c1", "expiresAt": "2026-10-08T01:00:00Z"})

    request = {"datasetId": "ds_1", "scope": {"kind": "dataset", "datasetId": "ds_1"}}
    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            estimate = client.exports.create({**request, "dryRun": True})
            started = client.exports.create(request, idempotency_key="k1")
            pages = list(client.exports.files("exp/1", set="train"))
    assert type(estimate).__name__ == "ExportEstimate" and estimate.bytes == 10
    assert type(started).__name__ == "Export" and started.id == "exp_1"
    assert seen[1].headers["Idempotency-Key"] == "k1"
    assert [p.files[0].path for p in pages] == ["f0", "fc1"]
    assert seen[2].url.raw_path.startswith(b"/api/v1/exports/exp%2F1/files")
    assert seen[3].url.params["cursor"] == "c1" and seen[3].url.params["set"] == "train"


def test_api_key_and_draft_bodies_use_wire_aliases() -> None:
    from simforge_cloud.models import CreateApiKeyRequest

    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        if request.method == "DELETE":
            return httpx.Response(204)
        if request.url.path == "/api/v1/api-keys":
            key = {"id": "key_1", "name": "ci", "scopes": ["runs:read"], "createdBy": "u", "createdAt": "t", "lastUsedAt": None, "expiresAt": None, "revokedAt": None}
            return httpx.Response(201, json={"key": key, "secret": "sfk_once"})
        return httpx.Response(200, json={"ok": True})

    with httpx.Client(transport=httpx.MockTransport(handler), base_url="http://mock") as http:
        with SimforgeClient(api_key="sfk_test", http_client=http) as client:
            body = CreateApiKeyRequest.model_validate({"name": "ci", "scopes": ["runs:read"], "expiresInDays": 30})
            created = client.api_keys.create(body)
            client.api_keys.revoke("key 1")
            result = client.authoring.discover("mv_1", kind="spawn", x=1.5)
    assert json.loads(seen[0].content) == {"name": "ci", "scopes": ["runs:read"], "expiresInDays": 30}
    assert created.key.id == "key_1"
    assert seen[1].url.raw_path == b"/api/v1/api-keys/key%201"
    assert seen[2].url.params["kind"] == "spawn" and "catalogId" not in seen[2].url.params
    assert result.root == {"ok": True}
