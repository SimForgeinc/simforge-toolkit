"""Scripted peers that speak the two wire contracts, for client boundary tests."""

from __future__ import annotations

import base64
import json
import os
import socket
import struct
import threading
from pathlib import Path
from typing import Any, Callable

import numpy as np

from simforge_rl import wire

FIELDS = ["target_speed_mps", "target_acceleration_mps2", "motion_direction", "throttle", "brake", "steer"]
HELLO = {
    "ok": True, "protocol": wire.PROTOCOL, "version": "test", "engineVersion": "0.0.0", "engineHz": 50, "ego": "ego",
    "decisionHz": 10, "actionWidth": len(FIELDS), "actionFields": FIELDS, "stateVectorSize": 3, "stateVectorEnabled": True,
    "objectFeatures": 5, "maxObjects": 2, "bevShape": None, "sensors": [],
}
CAMERA = {"sourceId": "front", "modality": "rgb", "width": 4, "height": 2, "passes": ["rgb", "depth"]}


class Tail:
    def __init__(self) -> None:
        self.data = bytearray()

    def put(self, array: np.ndarray) -> dict[str, Any]:
        while len(self.data) % 8:
            self.data.append(0)
        raw = np.ascontiguousarray(array).tobytes()
        ref = {"offset": len(self.data), "length": len(raw), "dtype": array.dtype.str, "shape": list(array.shape)}
        self.data.extend(raw)
        return ref


class Episode:
    """A tiny deterministic episode: speed tracks the last setpoint; three steps per episode."""

    def __init__(self, hello: dict[str, Any] | None = None) -> None:
        self.hello = dict(HELLO if hello is None else hello)
        self.t = 0
        self.seed: Any = None
        self.speed = 0.0
        self.actions: list[Any] = []

    def frame(self, extra: dict[str, Any]) -> tuple[dict[str, Any], bytes]:
        tail = Tail()
        observation = {
            "state_vector": tail.put(np.array([self.t, self.speed, float(self.seed or 0)], dtype="<f8")),
            "objects": tail.put(np.zeros((2, 5), dtype="<f4")),
        }
        header: dict[str, Any] = {"ok": True, "observation": observation, "t_s": self.t / 10,
                                  "info": {"t_s": self.t / 10, "ego": "ego", "object_ids": [], "reward_terms": {"progress": self.speed / 10, "comfort": -0.5},
                                           "events": [{"kind": "collision"}] if self.speed > 20 else [], "minima": [], "causal": {}}}
        if self.hello["sensors"]:
            header["sensors"] = {"front": {"rgb": tail.put(np.full((2, 4, 4), self.t, dtype="|u1")),
                                           "depth": tail.put(np.full((2, 4), 0.5, dtype="<f4"))}}
        header.update(extra)
        return header, bytes(tail.data)

    def handle(self, op: str, request: dict[str, Any], tail: bytes) -> tuple[dict[str, Any], bytes]:
        if op == "hello":
            return dict(self.hello), b""
        if op == "reset":
            self.t, self.speed, self.seed = 0, 0.0, request.get("seed")
            return self.frame({})
        if op == "step":
            action = request["action"]
            self.actions.append(action)
            if action is not None:
                if len(action) != len(FIELDS):
                    return {"ok": False, "code": "action_invalid", "reason": "width"}, b""
                if action[0] is not None:
                    self.speed = float(action[0])
            self.t += 1
            reward = self.speed / 10 - 0.5
            return self.frame({"reward": reward, "terminated": self.speed > 20, "truncated": self.t >= 3})
        if op == "observe":
            return self.frame({})
        if op == "checkpoint":
            t = Tail()
            ref = t.put(np.frombuffer(json.dumps([self.t, self.speed, self.seed]).encode(), dtype="|u1"))
            return {"ok": True, "checkpoint": ref}, bytes(t.data)
        if op == "restore":
            self.t, self.speed, self.seed = json.loads(wire.array(tail, request["checkpoint"]).tobytes())
            return self.frame({})
        return {"ok": False, "code": "unknown_op", "reason": op}, b""


def recv_exact(sock: socket.socket, n: int) -> bytes:
    data = bytearray()
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise ConnectionError
        data.extend(chunk)
    return bytes(data)


class FakeEnvServer:
    """An ``env serve`` socket: one :class:`Episode` per connection; ``mutate`` can corrupt responses."""

    def __init__(self, path: Path, *, hello: dict[str, Any] | None = None,
                 mutate: Callable[[dict[str, Any], bytes], tuple[dict[str, Any], bytes]] | None = None) -> None:
        self.path = os.fspath(path)
        self.hello, self.mutate = hello, mutate
        self.episodes: list[Episode] = []
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(self.path)
        self.listener.listen()
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self) -> None:
        while True:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            episode = Episode(self.hello)
            self.episodes.append(episode)
            threading.Thread(target=self._serve, args=(conn, episode), daemon=True).start()

    def _serve(self, conn: socket.socket, episode: Episode) -> None:
        with conn:
            while True:
                try:
                    (total,) = struct.unpack("<I", recv_exact(conn, 4))
                    request, tail = wire.decode_body(recv_exact(conn, total))
                except ConnectionError:
                    return
                header, out = episode.handle(request["op"], request, tail)
                header["i"] = request["i"]
                if self.mutate is not None:
                    header, out = self.mutate(header, out)
                conn.sendall(wire.encode(header, out))

    def close(self) -> None:
        self.listener.close()


class FakeRuntime:
    """A ``simforge-runtime serve`` peer over pipes: handshake, env.* over :class:`Episode`, scripted overrides."""

    def __init__(self, *, handshake: dict[str, Any] | None = None, override: Callable[[dict[str, Any]], dict[str, Any] | None] | None = None) -> None:
        self.handshake = handshake or {"runtimeProtocol": 1, "rendererProtocol": 5, "runtimeVersion": "test", "engineSemver": "0.0.0",
                                       "operations": ["handshake", "command", "env.create", "env.reset", "env.step", "env.observe",
                                                      "env.checkpoint", "env.restore", "env.close", "env.batch.create", "env.batch.reset",
                                                      "env.batch.step", "env.batch.observe", "env.batch.close", "world.create", "world.subscribe",
                                                      "world.unsubscribe", "world.restore", "world.snapshot"], "contractSha256": "0" * 64}
        self.override = override
        self.envs: dict[str, Episode] = {}
        self.batches: dict[str, dict[str, Any]] = {}
        self.requests: list[dict[str, Any]] = []
        c2s_r, c2s_w = os.pipe()
        s2c_r, s2c_w = os.pipe()
        self.client_reader, self.client_writer = os.fdopen(s2c_r, "rb"), os.fdopen(c2s_w, "wb")
        self._in, self._out = os.fdopen(c2s_r, "rb"), os.fdopen(s2c_w, "wb")
        threading.Thread(target=self._serve, daemon=True).start()

    def _reply(self, message: dict[str, Any]) -> None:
        self._out.write(json.dumps(message).encode() + b"\n")
        self._out.flush()

    def _serve(self) -> None:
        for line in self._in:
            request = json.loads(line)
            self.requests.append(request)
            base = {"schema": "simforge.runtime/v1", "id": request["id"], "op": request["op"]}
            scripted = self.override(request) if self.override else None
            if scripted is not None:
                self._reply({**base, **scripted})
                continue
            op, params = request["op"], request["params"]
            if op == "handshake":
                self._reply({**base, "ok": True, "result": self.handshake})
            elif op == "command":
                self._reply({**base, "ok": True, "result": {"value": {"argv": params["argv"]}, "exitCode": 2}})
            elif op.startswith("env.batch."):
                self._reply({**base, **self._batch(op, request["id"], params)})
            elif op.startswith("env."):
                if op == "env.create":
                    env_id = f"env-{len(self.envs) + 1}"
                    self.envs[env_id] = Episode()
                    header, tail = dict(HELLO), b""
                else:
                    env_id = params["envId"]
                    if env_id not in self.envs:
                        self._reply({**base, "ok": False, "error": {"code": "env_not_found", "reason": env_id, "exitCode": 1}})
                        continue
                    native = {"env.reset": "reset", "env.step": "step", "env.observe": "observe", "env.checkpoint": "checkpoint",
                              "env.restore": "restore", "env.close": "close"}[op]
                    if native == "close":
                        del self.envs[env_id]
                        header, tail = {"ok": True}, b""
                    elif native == "restore":
                        raw = base64.b64decode(params["checkpointBase64"])
                        header, tail = self.envs[env_id].handle("restore", {"checkpoint": {"offset": 0, "length": len(raw), "dtype": "|u1", "shape": [len(raw)]}}, raw)
                    else:
                        header, tail = self.envs[env_id].handle(native, params, b"")
                if header.get("ok") is False:
                    self._reply({**base, "ok": False, "error": {"code": header["code"], "reason": header["reason"], "exitCode": 1}})
                else:
                    self._reply({**base, "ok": True, "result": {"envId": env_id, "header": header, "tailBase64": base64.b64encode(tail).decode()}})
            else:
                self._reply({**base, "ok": False, "error": {"code": "unknown_op", "reason": op, "exitCode": 1}})

    def _batch(self, op: str, request_id: str, params: dict[str, Any]) -> dict[str, Any]:
        """``env.batch.*`` with the runtime's rules: reset before step, ended envs refuse step."""
        if op == "env.batch.create":
            batch_id = f"batch-{len(self.batches) + 1}"
            count = params["count"]
            self.batches[batch_id] = {"envs": [Episode() for _ in range(count)], "live": [False] * count}
            shards = params.get("shards", 1)
            return {"ok": True, "result": {"batchId": batch_id, "count": count, "shards": shards,
                                           "shardOf": [i * shards // count for i in range(count)], "observation": "state", "hello": dict(HELLO)}}
        batch = self.batches.get(params["batchId"])
        if batch is None:
            return {"ok": False, "error": {"code": "env_not_found", "reason": "batch", "exitCode": 1}}
        if op == "env.batch.close":
            del self.batches[params["batchId"]]
            return {"ok": True, "result": {"batchId": params["batchId"], "closed": True}}
        count = len(batch["envs"])
        mask = params.get("mask") or [True] * count
        results: list[Any] = []
        failed = []
        for k in range(count):
            if not mask[k]:
                results.append(None)
                continue
            episode = batch["envs"][k]
            if op == "env.batch.reset":
                header, tail = episode.handle("reset", {"seed": (params.get("seeds") or [None] * count)[k]}, b"")
                batch["live"][k] = True
            elif not batch["live"][k]:
                header, tail = {"ok": False, "op": "step", "code": "episode_not_reset", "reason": "reset first"}, b""
            else:
                header, tail = episode.handle("step", {"action": params["actions"][k]}, b"")
                if header.get("terminated") or header.get("truncated"):
                    batch["live"][k] = False
            header["i"] = f"{request_id}:{k}"
            if header.get("ok") is not True:
                failed.append(k)
            results.append({"envIndex": k, "ok": header.get("ok") is True, "header": header, "tailBase64": base64.b64encode(tail).decode()})
        return {"ok": True, "result": {"batchId": params["batchId"], "results": results, "failed": failed}}
