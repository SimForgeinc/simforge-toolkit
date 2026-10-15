"""Persistent-world types: sessions, checkpoints and truth subscriptions with bounded local queues."""

from __future__ import annotations

import base64
import binascii
import struct
import threading
from collections import deque
from dataclasses import dataclass, field
from typing import Any

from .wire import ProtocolError

TRUTH_OP = "world.truth"
CLOSED_OP = "world.subscriptionClosed"
CLOSE_REASONS = ("restored", "world_closed", "unsubscribed")


@dataclass(frozen=True)
class WorldSession:
    """``world.create``/``world.join`` identity; ``reconnect_token`` is a secret and stays out of ``repr``."""

    world_id: str
    client_id: str
    reconnect_token: str = field(repr=False)
    host: bool
    tick: int
    result: dict[str, Any] = field(repr=False)


@dataclass(frozen=True)
class WorldCheckpoint:
    """Opaque native world continuation bytes and their identity."""

    data: bytes = field(repr=False)
    tick: int
    digest: str
    base_input_hash: str


@dataclass(frozen=True)
class TruthDelivery:
    """One delivery: each packet is a complete native ``[u32le len][MsgPack TruthFrame]``, byte for byte."""

    sequence: int | None
    packets: list[bytes]
    queued: int
    dropped: int


def split_frames(frames_base64: Any) -> list[bytes]:
    """The framed packets in ``framesBase64`` (empty string = none), prefixes kept."""
    if not isinstance(frames_base64, str):
        raise ProtocolError("framesBase64 is not a string")
    try:
        data = base64.b64decode(frames_base64, validate=True)
    except binascii.Error as error:
        raise ProtocolError(f"framesBase64 is not valid base64: {error}") from error
    packets, offset = [], 0
    while offset < len(data):
        if offset + 4 > len(data):
            raise ProtocolError("truth stream ends inside a length prefix")
        (length,) = struct.unpack_from("<I", data, offset)
        offset += 4
        if offset + length > len(data):
            raise ProtocolError(f"truth frame of {length} bytes overruns the delivery")
        packets.append(data[offset - 4 : offset + length])
        offset += length
    return packets


def _count(message: dict[str, Any], key: str) -> int:
    value = message.get(key)
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ProtocolError(f"{key} must be a non-negative integer, got {value!r}")
    return value


class Subscription:
    """A ``world.subscribe`` stream: deliveries queue here (drop-oldest beyond ``capacity``)."""

    def __init__(self, world_id: str, subscription_id: str, result: dict[str, Any], capacity: int = 1024) -> None:
        self.world_id = world_id
        self.subscription_id = subscription_id
        self.result = result
        self._deliveries: deque[TruthDelivery] = deque()
        self._capacity = capacity
        self._ready = threading.Condition()
        self.next_sequence: int | None = None
        self.missing_sequences = 0
        self.runtime_dropped = 0
        self.local_dropped = 0
        self.closed_reason: str | None = None

    def _push(self, message: dict[str, Any]) -> None:
        if message.get("op") == CLOSED_OP:
            if message.get("reason") not in CLOSE_REASONS:
                raise ProtocolError(f"unknown subscription close reason {message.get('reason')!r}")
            self._close(message["reason"])
            return
        sequence = _count(message, "sequence")
        delivery = TruthDelivery(sequence, split_frames(message.get("framesBase64")), _count(message, "queued"), _count(message, "dropped"))
        with self._ready:
            if self.closed_reason is not None:
                return
            if self.next_sequence is not None and sequence != self.next_sequence:
                if sequence < self.next_sequence:
                    raise ProtocolError(f"subscription {self.subscription_id} sequence went back to {sequence}")
                self.missing_sequences += sequence - self.next_sequence
            if delivery.dropped < self.runtime_dropped:
                raise ProtocolError(f"subscription {self.subscription_id} dropped counter decreased")
            self.next_sequence = sequence + 1
            self.runtime_dropped = delivery.dropped
            if len(self._deliveries) >= self._capacity:
                self._deliveries.popleft()
                self.local_dropped += 1
            self._deliveries.append(delivery)
            self._ready.notify_all()

    def _close(self, reason: str) -> None:
        with self._ready:
            if self.closed_reason is None:
                self.closed_reason = reason
                if reason == "restored":
                    # Pre-restore truth belongs to an abandoned timeline; other closes stay drainable.
                    self._deliveries.clear()
            self._ready.notify_all()

    def get(self, timeout: float | None = None) -> TruthDelivery | None:
        """The oldest queued delivery; ``None`` on timeout or once closed and drained."""
        with self._ready:
            if not self._ready.wait_for(lambda: self._deliveries or self.closed_reason is not None, timeout):
                return None
            return self._deliveries.popleft() if self._deliveries else None

    def drain(self) -> list[TruthDelivery]:
        with self._ready:
            out = list(self._deliveries)
            self._deliveries.clear()
            return out
