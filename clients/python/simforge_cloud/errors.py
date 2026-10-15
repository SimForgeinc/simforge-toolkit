from __future__ import annotations

from typing import Any


class SimforgeApiError(Exception):
    """An error returned by the SimForge Cloud API envelope."""

    def __init__(
        self,
        code: str,
        reason: str,
        detail: Any = None,
        request_id: str | None = None,
        *,
        path: str | None = None,
    ) -> None:
        self.code = code
        self.reason = reason
        self.detail = detail
        self.request_id = request_id
        self.path = path
        super().__init__(f"{code}: {reason}")


class ArtifactIntegrityError(ValueError):
    """Raised when downloaded artifact bytes do not match the API sha256."""

    def __init__(self, artifact_id: str, expected: str, actual: str) -> None:
        self.artifact_id = artifact_id
        self.expected = expected
        self.actual = actual
        super().__init__(f"Artifact {artifact_id} sha256 mismatch: expected {expected}, got {actual}")
