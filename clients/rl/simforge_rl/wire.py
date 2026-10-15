"""``simforge.env-serve/v1`` framing: ``u32le total || u32le header_len || JSON header || binary tail``."""

from __future__ import annotations

import json
import math
import struct
from typing import Any, Mapping, Sequence

import numpy as np

PROTOCOL = "simforge.env-serve/v1"
MAX_MESSAGE_BYTES = 1 << 30
DTYPES = {"<f8": np.dtype("<f8"), "<f4": np.dtype("<f4"), "|u1": np.dtype("|u1")}


class ProtocolError(ConnectionError):
    """The peer sent bytes that violate the wire contract."""


class EnvServeError(RuntimeError):
    """A request the runtime refused: ``code`` is stable, ``reason`` human-readable."""

    def __init__(self, op: str, code: str, reason: str, detail: Any = None, exit_code: int | None = None) -> None:
        super().__init__(f"{op}: {code}: {reason}")
        self.op = op
        self.code = code
        self.reason = reason
        self.detail = detail
        self.exit_code = exit_code


def encode(header: Mapping[str, Any], tail: bytes = b"") -> bytes:
    body = json.dumps(header, allow_nan=False, separators=(",", ":")).encode()
    total = 4 + len(body) + len(tail)
    if total > MAX_MESSAGE_BYTES:
        raise ValueError(f"message of {total} bytes exceeds {MAX_MESSAGE_BYTES}")
    return struct.pack("<II", total, len(body)) + body + tail


def decode_body(body: bytes) -> tuple[dict[str, Any], bytes]:
    """Split one message body (after the total length) into header and tail."""
    if len(body) < 4:
        raise ProtocolError("message shorter than its header length field")
    (header_len,) = struct.unpack("<I", body[:4])
    if 4 + header_len > len(body):
        raise ProtocolError(f"header length {header_len} exceeds message of {len(body)} bytes")
    try:
        header = json.loads(body[4 : 4 + header_len])
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ProtocolError(f"header is not JSON: {error}") from error
    if not isinstance(header, dict):
        raise ProtocolError("header is not a JSON object")
    return header, body[4 + header_len :]


def _integer(value: Any, what: str, ref: Mapping[str, Any]) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ProtocolError(f"array descriptor {what} must be a non-negative integer in {dict(ref)!r}")
    return value


def array(tail: bytes, ref: Mapping[str, Any]) -> np.ndarray:
    """The tail array ``ref`` names, validated and copied."""
    if not isinstance(ref, Mapping) or not {"offset", "length", "dtype", "shape"} <= set(ref):
        raise ProtocolError(f"invalid array descriptor {ref!r}")
    dtype = DTYPES.get(ref["dtype"]) if isinstance(ref["dtype"], str) else None
    if dtype is None:
        raise ProtocolError(f"unsupported array dtype in {dict(ref)!r}")
    if not isinstance(ref["shape"], list):
        raise ProtocolError(f"array shape must be a list in {dict(ref)!r}")
    offset, length = _integer(ref["offset"], "offset", ref), _integer(ref["length"], "length", ref)
    shape = [_integer(d, "shape", ref) for d in ref["shape"]]
    if offset % 8:
        raise ProtocolError(f"array offset {offset} is not 8-byte aligned")
    if offset + length > len(tail):
        raise ProtocolError(f"array [{offset}, {offset + length}) exceeds tail of {len(tail)} bytes")
    if math.prod(shape) * dtype.itemsize != length:
        raise ProtocolError(f"shape {shape} of {dtype.str} does not span {length} bytes")
    return np.frombuffer(tail, dtype=dtype, count=length // dtype.itemsize, offset=offset).reshape(shape).copy()


def action_row(row: Sequence[float | None] | np.ndarray | None) -> list[float | None] | None:
    """The JSON action row: NaN and ``None`` mean unset; infinities are refused."""
    if row is None:
        return None
    out: list[float | None] = []
    for value in np.asarray([math.nan if v is None else v for v in row], dtype=np.float64).reshape(-1):
        if math.isnan(value):
            out.append(None)
        elif math.isinf(value):
            raise ValueError("action row contains an infinite value")
        else:
            out.append(float(value))
    return out
