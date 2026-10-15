from __future__ import annotations

import base64
import struct

import pytest

from simforge_rl import ProtocolError, RuntimeClient

from .fakes import FakeRuntime

WORLD = {"worldId": "w1", "clientId": "c1", "reconnectToken": "secret-token", "host": True, "tick": 0, "dtSec": 0.02,
         "pacing": "paused", "mapId": "rfs", "mapDigest": "0" * 64, "journalPath": "/j"}
SUBSCRIBED = {"subscriptionId": "s1", "dtSec": 0.02, "rateHz": 50, "capacity": 256, "channels": ["actors", "signals", "telemetry"]}


def framed(payload: bytes) -> bytes:
    return struct.pack("<I", len(payload)) + payload


def frames(*packets: bytes) -> str:
    return base64.b64encode(b"".join(framed(p) for p in packets)).decode()


def truth(sequence: int, *packets: bytes, dropped: int = 0, sub: str = "s1") -> dict:
    return {"schema": "simforge.runtime/v1", "op": "world.truth", "worldId": "w1", "subscriptionId": sub, "sequence": sequence,
            "framesBase64": frames(*packets), "queued": 0, "dropped": dropped}


class Script:
    """A fake runtime whose world ops reply from ``results`` after emitting ``before`` notifications."""

    def __init__(self) -> None:
        self.before: dict[str, list[dict]] = {}
        self.results = {"world.create": WORLD, "world.subscribe": SUBSCRIBED, "world.restore": {"tick": 7, "pacing": "paused"},
                        "world.snapshot": {"tS": 0.0, "tick": 0, "done": False, "actors": [], "signals": [], "pacing": "paused",
                                           "lagTicks": 0, "overloadCount": 0, "droppedSimTicks": 0, "haltedReason": None},
                        "world.unsubscribe": {"subscriptionId": "s1", "closed": True}}
        self.fake = FakeRuntime(override=self.answer)
        self.client = RuntimeClient(self.fake.client_reader, self.fake.client_writer)

    def answer(self, request: dict) -> dict | None:
        op = request["op"]
        for message in self.before.pop(op, []):
            self.fake._reply(message)
        return {"ok": True, "result": self.results[op]} if op in self.results else None


def test_session_secret_stays_out_of_repr() -> None:
    session = Script().client.world_create(["map"])
    assert session.reconnect_token == "secret-token" and "secret-token" not in repr(session)


def test_truth_racing_the_subscribe_response_and_interleaved_with_requests() -> None:
    script = Script()
    script.client.world_create(["map"])
    script.before["world.subscribe"] = [truth(0, b"a")]
    script.before["world.snapshot"] = [truth(1, b"b", b"c")]
    subscription = script.client.world_subscribe("w1")
    assert script.client.world_snapshot("w1")["tick"] == 0
    first, second = subscription.get(timeout=2), subscription.get(timeout=2)
    assert (first.sequence, first.packets) == (0, [framed(b"a")])
    assert (second.sequence, second.packets) == (1, [framed(b"b"), framed(b"c")])


def test_gaps_runtime_drops_and_local_overflow_are_counted() -> None:
    script = Script()
    script.client.world_create(["map"])
    subscription = script.client.world_subscribe("w1", capacity=2)
    script.before["world.snapshot"] = [truth(0, b"a"), truth(3, b"b", dropped=5), truth(4, b"c", dropped=5)]
    script.client.world_snapshot("w1")
    script.client.world_snapshot("w1")
    assert subscription.missing_sequences == 2 and subscription.runtime_dropped == 5
    assert subscription.local_dropped == 1
    assert [d.sequence for d in subscription.drain()] == [3, 4]


def test_restore_ends_subscriptions_and_ignores_stale_truth() -> None:
    script = Script()
    script.client.world_create(["map"])
    subscription = script.client.world_subscribe("w1")
    script.client.world_restore("w1", b"checkpoint")
    assert subscription.closed_reason == "restored"
    script.before["world.snapshot"] = [truth(9, b"late")]
    script.client.world_snapshot("w1")
    assert subscription.get(timeout=0.2) is None


def test_corrupt_truth_framing_fails_the_connection() -> None:
    script = Script()
    script.client.world_create(["map"])
    script.client.world_subscribe("w1")
    bad = truth(0)
    bad["framesBase64"] = base64.b64encode(struct.pack("<I", 10) + b"abc").decode()
    script.before["world.snapshot"] = [bad]
    with pytest.raises(ProtocolError, match="overruns"):
        script.client.world_snapshot("w1")


def test_after_a_fatal_stream_error_no_further_request_is_sent() -> None:
    script = Script()
    script.client.world_create(["map"])
    script.client.world_subscribe("w1")
    bad = truth(0)
    bad["framesBase64"] = "@@@@"
    script.before["world.snapshot"] = [bad]
    with pytest.raises(ProtocolError):
        script.client.world_snapshot("w1")
    sent = len(script.fake.requests)
    with pytest.raises(ProtocolError):
        script.client.world_snapshot("w1")
    assert len(script.fake.requests) == sent and not script.client.alive


def test_truth_for_unknown_subscriptions_is_dropped_without_growth() -> None:
    script = Script()
    script.client.world_create(["map"])
    script.before["world.snapshot"] = [truth(k, b"x", sub="ghost") for k in range(5000)]
    assert script.client.world_snapshot("w1")["tick"] == 0
    subscription = script.client.world_subscribe("w1")
    assert subscription.get(timeout=0.1) is None


def test_connection_loss_is_not_reported_as_a_closed_world() -> None:
    script = Script()
    script.client.world_create(["map"])
    subscription = script.client.world_subscribe("w1")
    script.fake._out.close()
    assert subscription.get(timeout=2) is None
    assert subscription.closed_reason == "connection_lost"


def test_restore_never_ends_a_subscription_opened_concurrently() -> None:
    import threading
    import time

    script = Script()
    script.client.world_create(["map"])
    answer = script.answer

    def slow(request):
        if request["op"] == "world.restore":
            time.sleep(0.2)
        return answer(request)

    script.fake.override = slow
    forget = script.client._forget

    def late_forget(*args, **kwargs):
        time.sleep(0.2)  # the restorer is descheduled between its response and its registry update
        forget(*args, **kwargs)

    script.client._forget = late_forget
    restorer = threading.Thread(target=script.client.world_restore, args=("w1", b"checkpoint"))
    restorer.start()
    time.sleep(0.05)
    subscription = script.client.world_subscribe("w1")
    restorer.join()
    assert subscription.closed_reason is None


def test_restore_discards_queued_pre_restore_truth_but_unsubscribe_keeps_it() -> None:
    script = Script()
    script.client.world_create(["map"])
    restored = script.client.world_subscribe("w1")
    script.before["world.snapshot"] = [truth(k, b"old") for k in range(3)]
    script.client.world_snapshot("w1")
    script.before["world.restore"] = [truth(3, b"late")]
    script.client.world_restore("w1", b"checkpoint")
    assert restored.closed_reason == "restored"
    assert restored.get(timeout=0.1) is None and restored.drain() == []

    script.results["world.subscribe"] = {**SUBSCRIBED, "subscriptionId": "s2"}
    script.results["world.unsubscribe"] = {"subscriptionId": "s2", "closed": True}
    kept = script.client.world_subscribe("w1")
    script.before["world.snapshot"] = [truth(0, b"kept", sub="s2")]
    script.client.world_snapshot("w1")
    script.client.world_unsubscribe(kept)
    assert [d.packets for d in kept.drain()] == [[framed(b"kept")]]
