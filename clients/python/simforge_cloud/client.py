from __future__ import annotations

import hashlib
import os
import tempfile
import time
import uuid
from collections.abc import Iterator, Mapping
from pathlib import Path
from typing import Any, Generic, TypeVar
from urllib.parse import quote, urljoin

import httpx
from pydantic import BaseModel

from .errors import ArtifactIntegrityError, SimforgeApiError
from .models import (
    ApiKeyListResponse,
    Artifact,
    ArtifactDownload,
    AuthoringCatalogue,
    AuthoringHttpDraftTransactionResult,
    AuthoringResult,
    CreateApiKeyResponse,
    DatasetDetail,
    DatasetSummary,
    DraftReceipt,
    DraftRevisionReceipt,
    DryRunResponse,
    Export,
    ExportEstimate,
    ExportFilesPage,
    HealthResponse,
    MapSummary,
    MeResponse,
    Recording,
    RecordingDownload,
    Run,
    RunEvent,
    ScenarioDetail,
    ScenarioSummary,
    UploadResponse,
    UsageResponse,
    VehicleSummary,
    WorkspacesResponse,
)

T = TypeVar("T", bound=BaseModel)
_TERMINAL_RUN_STATUSES = {"succeeded", "partial", "failed", "cancelled"}


def _model_validate(model: type[T], value: Any) -> T:
    return model.model_validate(value)


class _PageIterator(Generic[T], Iterator[T]):
    def __init__(
        self,
        client: SimforgeClient,
        path: str,
        params: dict[str, Any],
        model: type[T],
    ) -> None:
        self._client = client
        self._path = path
        self._params = params
        self._model = model
        self._items: Iterator[T] | None = None
        self._next_cursor: str | None = None
        self._finished = False

    def __iter__(self) -> _PageIterator[T]:
        return self

    def __next__(self) -> T:
        while True:
            if self._items is not None:
                try:
                    return next(self._items)
                except StopIteration:
                    self._items = None
            if self._finished:
                raise StopIteration
            page = self._client._request("GET", self._path, params=self._params)
            raw_items = page.get("data")
            if not isinstance(raw_items, list):
                raise SimforgeApiError(
                    "invalid_response",
                    "The API returned a page without a data array.",
                    detail=page,
                    request_id=self._client.last_request_id,
                )
            self._items = iter(_model_validate(self._model, item) for item in raw_items)
            cursor = page.get("nextCursor")
            self._next_cursor = cursor if isinstance(cursor, str) and cursor else None
            if self._next_cursor is None:
                self._finished = True
            else:
                self._params["cursor"] = self._next_cursor


class _ScenariosResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def list(self, *, cursor: str | None = None, limit: int | None = None) -> Iterator[ScenarioSummary]:
        return _PageIterator(self._client, "/scenarios", _params(cursor, limit), ScenarioSummary)

    def get(self, scenario_id: str) -> ScenarioDetail:
        return self._client._parse(ScenarioDetail, self._client._request("GET", f"/scenarios/{quote(scenario_id, safe='')}"))


class _MapsResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def list(self, *, cursor: str | None = None, limit: int | None = None) -> Iterator[MapSummary]:
        return _PageIterator(self._client, "/maps", _params(cursor, limit), MapSummary)

    def get(self, map_id: str) -> MapSummary:
        return self._client._parse(MapSummary, self._client._request("GET", f"/maps/{quote(map_id, safe='')}"))


class _VehiclesResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def list(self, *, cursor: str | None = None, limit: int | None = None) -> Iterator[VehicleSummary]:
        """Every vehicle a scenario can bind by catalog id: the shared catalog's, then the workspace's own."""
        return _PageIterator(self._client, "/vehicles", _params(cursor, limit), VehicleSummary)

    def create(
        self,
        *,
        id: str,
        upload_id: str,
        vehicle_class: str,
        license: str,
        attribution: str,
        name: str | None = None,
        dims: tuple[float, float, float] | None = None,
        license_text: str | None = None,
        source: str | None = None,
    ) -> VehicleSummary:
        """Store the model an upload of kind ``vehicle-model`` holds as the workspace's own ``workspace.<id>``."""
        body: dict[str, Any] = {"id": id, "uploadId": upload_id, "vehicleClass": vehicle_class, "license": license, "attribution": attribution}
        optional = {"name": name, "dims": list(dims) if dims else None, "licenseText": license_text, "source": source}
        body.update({key: value for key, value in optional.items() if value is not None})
        return self._client._parse(VehicleSummary, self._client._request("POST", "/vehicles", json=body))

    def delete(self, catalog_id: str) -> None:
        """Retire one of the workspace's own vehicles (``workspace.<id>``)."""
        self._client._request("DELETE", f"/vehicles/{quote(catalog_id, safe='')}")


class _DatasetsResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def list(self, *, cursor: str | None = None, limit: int | None = None) -> Iterator[DatasetSummary]:
        return _PageIterator(self._client, "/datasets", _params(cursor, limit), DatasetSummary)

    def get(self, dataset_id: str) -> DatasetDetail:
        return self._client._parse(DatasetDetail, self._client._request("GET", f"/datasets/{quote(dataset_id, safe='')}"))


class _RunsResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def create(
        self,
        type: str,
        input: Any,
        dry_run: bool = False,
        idempotency_key: str | None = None,
        max_cost_cents: int | None = None,
    ) -> Run | DryRunResponse:
        payload = {"type": type, "input": input}
        if dry_run:
            payload["dryRun"] = True
        if max_cost_cents is not None:
            payload["maxCostCents"] = max_cost_cents
        raw = self._client._request(
            "POST",
            "/runs",
            json=payload,
            idempotent=True,
            idempotency_key=idempotency_key,
        )
        if raw.get("dryRun") is True:
            return self._client._parse(DryRunResponse, raw)
        return self._client._parse(Run, raw)

    def get(self, run_id: str) -> Run:
        return self._client._parse(Run, self._client._request("GET", f"/runs/{quote(run_id, safe='')}"))

    def list(
        self,
        *,
        cursor: str | None = None,
        limit: int | None = None,
        status: str | None = None,
        type: str | None = None,
    ) -> Iterator[Run]:
        params = _params(cursor, limit)
        if status is not None:
            params["status"] = status
        if type is not None:
            params["type"] = type
        return _PageIterator(self._client, "/runs", params, Run)

    def events(
        self,
        run_id: str,
        *,
        after: int | None = None,
        limit: int | None = None,
    ) -> Iterator[RunEvent]:
        current_after = after
        while True:
            params: dict[str, Any] = {}
            if current_after is not None:
                params["after"] = current_after
            if limit is not None:
                params["limit"] = limit
            page = self._client._request("GET", f"/runs/{quote(run_id, safe='')}/events", params=params)
            data = page.get("data")
            if not isinstance(data, list):
                raise SimforgeApiError(
                    "invalid_response",
                    "The API returned run events without a data array.",
                    detail=page,
                    request_id=self._client.last_request_id,
                )
            for event in data:
                yield self._client._parse(RunEvent, event)
            next_after = page.get("nextAfter")
            if not isinstance(next_after, int) or not data or next_after == current_after:
                return
            current_after = next_after

    def cancel(self, run_id: str) -> Run:
        return self._client._parse(Run, self._client._request("POST", f"/runs/{quote(run_id, safe='')}/cancel"))

    def wait(self, run_id: str, timeout: float, *, poll_interval: float = 0.5) -> Run:
        if timeout < 0:
            raise ValueError("timeout must be non-negative")
        deadline = time.monotonic() + timeout
        after = 0
        while True:
            events = self._client._request(
                "GET",
                f"/runs/{quote(run_id, safe='')}/events",
                params={"after": after},
            )
            next_after = events.get("nextAfter")
            if isinstance(next_after, int):
                after = next_after
            run = self.get(run_id)
            if run.status in _TERMINAL_RUN_STATUSES:
                return run
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"Timed out waiting for run {run_id}")
            time.sleep(min(poll_interval, remaining))


class _ArtifactsResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def list(self, run_id: str) -> Iterator[Artifact]:
        raw = self._client._request("GET", f"/runs/{quote(run_id, safe='')}/artifacts")
        data = raw.get("data")
        if not isinstance(data, list):
            raise SimforgeApiError(
                "invalid_response",
                "The API returned artifacts without a data array.",
                detail=raw,
                request_id=self._client.last_request_id,
            )
        return iter(self._client._parse(Artifact, item) for item in data)

    def download(self, artifact_id: str, dest: str | os.PathLike[str]) -> Path:
        descriptor = self._client._parse(
            ArtifactDownload,
            self._client._request("GET", f"/artifacts/{quote(artifact_id, safe='')}/download"),
        )
        destination = Path(dest)
        temporary: Path | None = None
        digest = hashlib.sha256()
        try:
            with tempfile.NamedTemporaryFile(
                mode="wb",
                dir=destination.parent,
                prefix=f".{destination.name}.",
                suffix=".part",
                delete=False,
            ) as output:
                temporary = Path(output.name)
                url = urljoin(str(self._client._http.base_url), descriptor.url)
                if descriptor.url.startswith(("http://", "https://")):
                    download = httpx.stream("GET", url, timeout=self._client._timeout)
                else:
                    download = self._client._http.stream("GET", descriptor.url, timeout=self._client._timeout)
                with download as response:
                    if response.status_code < 200 or response.status_code >= 300:
                        raise SimforgeApiError(
                            f"http_{response.status_code}",
                            f"Artifact download returned HTTP {response.status_code}.",
                            request_id=response.headers.get("X-Request-Id"),
                        )
                    for chunk in response.iter_bytes():
                        digest.update(chunk)
                        output.write(chunk)
                actual = digest.hexdigest()
                expected = descriptor.sha256.lower()
                if actual != expected:
                    raise ArtifactIntegrityError(artifact_id, expected, actual)
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, destination)
            temporary = None
            return destination
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)


class _UploadsResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def create(self, kind: str, sha256: str, size_bytes: int, idempotency_key: str | None = None) -> UploadResponse:
        raw = self._client._request(
            "POST",
            "/uploads",
            json={"kind": kind, "sha256": sha256, "sizeBytes": size_bytes},
            idempotent=True,
            idempotency_key=idempotency_key,
        )
        return self._client._parse(UploadResponse, raw)


class _MeResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def get(self) -> MeResponse:
        return self._client._parse(MeResponse, self._client._request("GET", "/me"))


class _WorkspacesResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def list(self) -> WorkspacesResponse:
        return self._client._parse(WorkspacesResponse, self._client._request("GET", "/workspaces"))


class _UsageResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def get(self, *, from_: str | None = None, to: str | None = None) -> UsageResponse:
        params: dict[str, Any] = {}
        if from_ is not None:
            params["from"] = from_
        if to is not None:
            params["to"] = to
        return self._client._parse(UsageResponse, self._client._request("GET", "/usage", params=params))


class _HealthResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def get(self) -> HealthResponse:
        return self._client._parse(HealthResponse, self._client._request("GET", "/health"))


class _ApiKeysResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def list(self) -> ApiKeyListResponse:
        return self._client._parse(ApiKeyListResponse, self._client._request("GET", "/api-keys"))

    def create(self, body: Mapping[str, Any] | BaseModel) -> CreateApiKeyResponse:
        """The response carries the secret once; never log it."""
        return self._client._parse(CreateApiKeyResponse, self._client._request("POST", "/api-keys", json=_body(body)))

    def revoke(self, key_id: str) -> None:
        self._client._request("DELETE", f"/api-keys/{quote(key_id, safe='')}")


class _RecordingsResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def create(self, upload_id: str) -> Recording:
        return self._client._parse(Recording, self._client._request("POST", "/recordings", json={"uploadId": upload_id}))

    def get(self, sha256: str) -> RecordingDownload:
        return self._client._parse(RecordingDownload, self._client._request("GET", f"/recordings/{quote(sha256, safe='')}"))


class _ExportsResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def create(self, body: Mapping[str, Any] | BaseModel, idempotency_key: str | None = None) -> Export | ExportEstimate:
        """A request with ``dryRun`` answers an estimate; otherwise the started export."""
        raw = self._client._request("POST", "/exports", json=_body(body), idempotent=True, idempotency_key=idempotency_key)
        if isinstance(raw, dict) and raw.get("dryRun") is True:
            return self._client._parse(ExportEstimate, raw)
        return self._client._parse(Export, raw)

    def list(self, *, cursor: str | None = None, dataset_id: str | None = None) -> Iterator[Export]:
        params = _params(cursor, None)
        if dataset_id is not None:
            params["datasetId"] = dataset_id
        return _PageIterator(self._client, "/exports", params, Export)

    def get(self, export_id: str) -> Export:
        return self._client._parse(Export, self._client._request("GET", f"/exports/{quote(export_id, safe='')}"))

    def files(self, export_id: str, *, set: str | None = None, limit: int | None = None) -> Iterator[ExportFilesPage]:
        """Each page of signed file URLs; follows ``nextCursor`` until exhausted."""
        params = _params(None, limit)
        if set is not None:
            params["set"] = set
        while True:
            page = self._client._parse(
                ExportFilesPage,
                self._client._request("GET", f"/exports/{quote(export_id, safe='')}/files", params=params),
            )
            yield page
            if not page.next_cursor:
                return
            params["cursor"] = page.next_cursor

    def retry(self, export_id: str) -> Export:
        """Package the scenarios the export left out again."""
        return self._client._parse(Export, self._client._request("POST", f"/exports/{quote(export_id, safe='')}/retry", json={"mode": "retry"}))

    def cancel(self, export_id: str) -> Export:
        return self._client._parse(Export, self._client._request("POST", f"/exports/{quote(export_id, safe='')}/cancel"))


class _DraftsResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def create(self, body: Mapping[str, Any] | BaseModel) -> DraftReceipt:
        return self._client._parse(DraftReceipt, self._client._request("POST", "/drafts", json=_body(body)))

    def get(self, draft_id: str) -> DraftReceipt:
        return self._client._parse(DraftReceipt, self._client._request("GET", f"/drafts/{quote(draft_id, safe='')}"))

    def transact(self, draft_id: str, body: Mapping[str, Any] | BaseModel) -> AuthoringHttpDraftTransactionResult:
        raw = self._client._request("POST", f"/drafts/{quote(draft_id, safe='')}/transactions", json=_body(body))
        return self._client._parse(AuthoringHttpDraftTransactionResult, raw)

    def save(self, draft_id: str, body: Mapping[str, Any] | BaseModel) -> DraftRevisionReceipt:
        raw = self._client._request("POST", f"/drafts/{quote(draft_id, safe='')}/save", json=_body(body))
        return self._client._parse(DraftRevisionReceipt, raw)

    def freeze_revision(self, draft_id: str, body: Mapping[str, Any] | BaseModel) -> DraftRevisionReceipt:
        raw = self._client._request("POST", f"/drafts/{quote(draft_id, safe='')}/freeze-revision", json=_body(body))
        return self._client._parse(DraftRevisionReceipt, raw)

    def validate(self, draft_id: str, body: Mapping[str, Any] | BaseModel) -> AuthoringResult:
        raw = self._client._request("POST", f"/drafts/{quote(draft_id, safe='')}/validate", json=_body(body))
        return self._client._parse(AuthoringResult, raw)


class _AuthoringResource:
    def __init__(self, client: SimforgeClient) -> None:
        self._client = client

    def catalogue(self) -> AuthoringCatalogue:
        return self._client._parse(AuthoringCatalogue, self._client._request("GET", "/authoring/catalogue"))

    def capabilities(self) -> AuthoringResult:
        return self._client._parse(AuthoringResult, self._client._request("GET", "/authoring/capabilities"))

    def spawn(self, map_version_id: str, body: Mapping[str, Any] | BaseModel) -> AuthoringResult:
        raw = self._client._request("POST", f"/authoring/maps/{quote(map_version_id, safe='')}/spawn", json=_body(body))
        return self._client._parse(AuthoringResult, raw)

    def discover(
        self,
        map_version_id: str,
        *,
        kind: str | None = None,
        limit: int | None = None,
        x: float | None = None,
        z: float | None = None,
        catalog_id: str | None = None,
    ) -> AuthoringResult:
        query = {"kind": kind, "limit": limit, "x": x, "z": z, "catalogId": catalog_id}
        params = {key: value for key, value in query.items() if value is not None}
        raw = self._client._request("GET", f"/authoring/maps/{quote(map_version_id, safe='')}/discovery", params=params)
        return self._client._parse(AuthoringResult, raw)


class SimforgeClient:
    def __init__(
        self,
        *,
        api_key: str | None = None,
        base_url: str | None = None,
        workspace_id: str | None = None,
        timeout: float | httpx.Timeout = 30.0,
        http_client: httpx.Client | None = None,
    ) -> None:
        resolved_key = api_key if api_key is not None else os.getenv("SIMFORGE_API_KEY")
        if not resolved_key:
            raise ValueError("api_key is required or SIMFORGE_API_KEY must be set")
        resolved_base = base_url or os.getenv("SIMFORGE_API_BASE_URL") or "https://simforge.ai"
        self._timeout = timeout
        self.workspace_id = workspace_id
        self.last_request_id: str | None = None
        self._owns_http_client = http_client is None
        self._http = http_client or httpx.Client(
            base_url=resolved_base.rstrip("/"),
            timeout=timeout,
            headers={"Authorization": f"Bearer {resolved_key}"},
        )
        self._http.headers.setdefault("Authorization", f"Bearer {resolved_key}")
        self.scenarios = _ScenariosResource(self)
        self.maps = _MapsResource(self)
        self.vehicles = _VehiclesResource(self)
        self.datasets = _DatasetsResource(self)
        self.runs = _RunsResource(self)
        self.artifacts = _ArtifactsResource(self)
        self.uploads = _UploadsResource(self)
        self.me = _MeResource(self)
        self.workspaces = _WorkspacesResource(self)
        self.usage = _UsageResource(self)
        self.health = _HealthResource(self)
        self.api_keys = _ApiKeysResource(self)
        self.recordings = _RecordingsResource(self)
        self.exports = _ExportsResource(self)
        self.drafts = _DraftsResource(self)
        self.authoring = _AuthoringResource(self)

    def close(self) -> None:
        if self._owns_http_client:
            self._http.close()

    def __enter__(self) -> SimforgeClient:
        return self

    def __exit__(self, exc_type: Any, exc_value: Any, traceback: Any) -> None:
        self.close()

    def _parse(self, model: type[T], raw: Any) -> T:
        return _model_validate(model, raw)

    def _request(
        self,
        method: str,
        path: str,
        *,
        params: Mapping[str, Any] | None = None,
        json: Any = None,
        idempotent: bool = False,
        idempotency_key: str | None = None,
    ) -> Any:
        headers: dict[str, str] = {}
        if self.workspace_id:
            headers["X-SimForge-Workspace-Id"] = self.workspace_id
        if idempotent:
            headers["Idempotency-Key"] = idempotency_key or f"sdk-{uuid.uuid4()}"
        try:
            response = self._http.request(method, f"/api/v1{path}", params=params, json=json, headers=headers)
        except httpx.HTTPError as error:
            raise SimforgeApiError("transport_error", str(error), request_id=None) from error
        self.last_request_id = response.headers.get("X-Request-Id")
        try:
            raw = response.json() if response.content else None
        except ValueError:
            raw = None
        if response.status_code < 200 or response.status_code >= 300:
            error_payload = raw.get("error") if isinstance(raw, dict) else None
            if isinstance(error_payload, dict):
                raise SimforgeApiError(
                    str(error_payload.get("code", "api_error")),
                    str(error_payload.get("reason", "The API request failed.")),
                    error_payload.get("detail"),
                    error_payload.get("requestId") or self.last_request_id,
                    path=error_payload.get("path"),
                )
            raise SimforgeApiError(
                f"http_{response.status_code}",
                response.text or f"The API returned HTTP {response.status_code}.",
                request_id=self.last_request_id,
            )
        return raw


def _params(cursor: str | None, limit: int | None) -> dict[str, Any]:
    params: dict[str, Any] = {}
    if cursor is not None:
        params["cursor"] = cursor
    if limit is not None:
        params["limit"] = limit
    return params


def _body(body: Mapping[str, Any] | BaseModel) -> Any:
    if isinstance(body, BaseModel):
        return body.model_dump(mode="json", by_alias=True, exclude_none=True)
    return dict(body)
