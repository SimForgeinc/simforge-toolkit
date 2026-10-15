"""SimForge runtime protocol v1 client."""

from __future__ import annotations

import base64
import binascii
import json
import os
import queue
import socket as _socket
import subprocess
import threading
from dataclasses import dataclass
from typing import IO, Any, Literal, Mapping, Sequence

import numpy as np

from .server import find_runtime, runtime_env
from .wire import EnvServeError, ProtocolError, action_row
from .world import CLOSED_OP, TRUTH_OP, Subscription, TruthDelivery, WorldCheckpoint, WorldSession, split_frames

MAX_BATCH_ENVS = 64
MAX_EARLY_NOTIFICATIONS = 4096
SCHEMA = "simforge.runtime/v1"
RUNTIME_PROTOCOL = 1
RENDERER_PROTOCOL = 5
MAX_LINE_BYTES = 64 << 20
RenderMode = Literal["2d", "schematic", "3d"]
ENV_OPS = ("env.create", "env.reset", "env.step", "env.observe", "env.checkpoint", "env.restore", "env.close")


class RuntimeProtocolError(EnvServeError):
    """A refusal in the runtime protocol's error envelope."""

    def __init__(self, op: str, code: str, reason: str, detail: Any = None, exit_code: int | None = None, path: str | None = None) -> None:
        super().__init__(op, code, reason, detail, exit_code)
        self.path = path


@dataclass(frozen=True)
class CommandResult:
    """An operation's unchanged result document; ``exit_code`` 2 means completed with findings."""

    value: Any
    exit_code: int


@dataclass(frozen=True)
class RuntimeBatch:
    """An ``env.batch.create`` handle: ``count`` envs over ``shards`` runtime worker threads."""

    batch_id: str
    count: int
    shards: int
    shard_of: tuple[int, ...]
    hello: dict[str, Any]


@dataclass(frozen=True)
class BatchRow:
    """One env's native answer; ``ok`` False carries the env-serve error header."""

    env_index: int
    ok: bool
    header: dict[str, Any]
    tail: bytes


@dataclass(frozen=True)
class BatchResults:
    """Rows in env order (``None`` where masked) and the indices that failed."""

    rows: list[BatchRow | None]
    failed: tuple[int, ...]


def _shutdown(sock: Any) -> None:
    try:
        sock.shutdown(_socket.SHUT_RDWR)
    except OSError:
        pass


def _index(name: str, value: Any, low: int, high: int) -> int:
    """An integer (bool and fractional values refused) within ``[low, high]``."""
    if isinstance(value, (bool, np.bool_)) or not isinstance(value, (int, np.integer)):
        raise TypeError(f"{name} must be an integer, got {value!r}")
    if not low <= int(value) <= high:
        raise ValueError(f"{name} must be in {low}..={high}, got {value}")
    return int(value)


def _b64decode(text: Any) -> bytes:
    if not isinstance(text, str):
        raise ProtocolError("tailBase64 is not a string")
    try:
        return base64.b64decode(text, validate=True)
    except binascii.Error as error:
        raise ProtocolError(f"tailBase64 is not valid base64: {error}") from error


class RuntimeClient:
    """One runtime connection; requests are serialized and ids must echo."""

    def __init__(
        self,
        reader: IO[bytes],
        writer: IO[bytes],
        *,
        process: subprocess.Popen[bytes] | None = None,
        closer: Any = None,
        expected_contract_sha256: str | None = None,
    ) -> None:
        self._reader = reader
        self._writer = writer
        self._process = process
        self._closer = closer
        self._lock = threading.RLock()
        self._next = 0
        self._closed = False
        self._failure: BaseException | None = None
        self._responses: queue.Queue[Any] = queue.Queue()
        self._subscriptions: dict[tuple[str, str], Subscription] = {}
        self._subscribing: dict[str, int] = {}
        self._pending: list[dict[str, Any]] = []
        self._routing = threading.Lock()
        self._thread = threading.Thread(target=self._read, name="simforge-runtime-reader", daemon=True)
        self._thread.start()
        self.handshake = self._handshake(expected_contract_sha256)
        self.operations = frozenset(self.handshake["operations"])

    def _read(self) -> None:
        """Route notifications to subscriptions; queue everything else for the waiting request."""
        while True:
            try:
                raw = self._reader.readline(MAX_LINE_BYTES + 1)
                if not raw:
                    raise ConnectionError("the runtime closed the connection")
                if not raw.endswith(b"\n"):
                    raise ProtocolError(f"runtime message exceeds {MAX_LINE_BYTES} bytes or is unterminated")
                try:
                    message = json.loads(raw)
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise ProtocolError(f"runtime message is not JSON: {error}") from error
                if isinstance(message, dict) and "id" not in message and message.get("op") in (TRUTH_OP, CLOSED_OP):
                    self._route(message)
                else:
                    self._responses.put(message)
            except (OSError, ValueError, ConnectionError, ProtocolError) as error:
                failure = error if isinstance(error, (ConnectionError, ProtocolError)) else ConnectionError(f"runtime connection failed: {error}")
                self._failure = failure
                self._responses.put(failure)
                break
        with self._routing:
            for subscription in self._subscriptions.values():
                subscription._close("connection_lost")
            self._subscriptions.clear()
            self._pending.clear()

    def _route(self, message: dict[str, Any]) -> None:
        if message.get("schema") != SCHEMA or not isinstance(message.get("worldId"), str) or not isinstance(message.get("subscriptionId"), str):
            raise ProtocolError(f"malformed {message.get('op')} notification")
        key = (message["worldId"], message["subscriptionId"])
        with self._routing:
            subscription = self._subscriptions.get(key)
            if subscription is None:
                # Only a subscribe in flight can own an unknown id; anything else is stale and dropped.
                if self._subscribing.get(key[0]):
                    if len(self._pending) >= MAX_EARLY_NOTIFICATIONS:
                        raise ProtocolError("too many notifications ahead of a world.subscribe response")
                    self._pending.append(message)
                return
            subscription._push(message)
            if subscription.closed_reason is not None:
                del self._subscriptions[key]

    @classmethod
    def spawn(
        cls,
        binary: str | os.PathLike[str] | None = None,
        *args: str,
        env: Mapping[str, str] | None = None,
        expected_contract_sha256: str | None = None,
    ) -> RuntimeClient:
        """Start ``simforge-runtime serve --stdio``: ``binary``, else ``$SIMFORGE_RUNTIME_BIN``, else the installed runtime, else PATH."""
        exe = find_runtime(binary)
        process = subprocess.Popen(
            [exe, *(args or ("serve", "--stdio"))],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            env=runtime_env(exe, env),
        )
        assert process.stdin is not None and process.stdout is not None
        try:
            return cls(process.stdout, process.stdin, process=process, expected_contract_sha256=expected_contract_sha256)
        except BaseException:
            process.kill()
            process.wait()
            raise

    @classmethod
    def connect(cls, path: str | os.PathLike[str] | None = None, *, timeout: float | None = None, expected_contract_sha256: str | None = None) -> RuntimeClient:
        """Connect to a same-user runtime socket (``path``, else ``$SIMFORGE_RUNTIME_SOCKET``)."""
        target = os.fspath(path) if path is not None else os.environ.get("SIMFORGE_RUNTIME_SOCKET")
        if not target:
            raise FileNotFoundError("no runtime socket: pass path= or set SIMFORGE_RUNTIME_SOCKET")
        sock = _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM)
        sock.settimeout(timeout)
        try:
            sock.connect(target)
            stream = sock.makefile("rwb")
        except BaseException:
            sock.close()
            raise
        try:
            return cls(stream, stream, closer=sock, expected_contract_sha256=expected_contract_sha256)
        except BaseException:
            _shutdown(sock)
            stream.close()
            sock.close()
            raise

    @property
    def alive(self) -> bool:
        return not self._closed and self._failure is None and (self._process is None or self._process.poll() is None)

    def request(self, op: str, params: Mapping[str, Any]) -> dict[str, Any]:
        """Send one request; return its ``result`` or raise its error."""
        with self._lock:
            if self._closed:
                raise ConnectionError("runtime connection is closed")
            if self._failure is not None:
                raise self._failure
            self._next += 1
            request_id = f"r{self._next}"
            line = json.dumps({"schema": SCHEMA, "id": request_id, "op": op, "params": dict(params)}, allow_nan=False, separators=(",", ":")).encode()
            if len(line) + 1 > MAX_LINE_BYTES:
                raise ValueError(f"request of {len(line) + 1} bytes exceeds {MAX_LINE_BYTES}")
            self._writer.write(line + b"\n")
            self._writer.flush()
            raw = self._responses.get()
        if isinstance(raw, BaseException):
            self._responses.put(raw)
            raise raw
        response = raw
        if not isinstance(response, dict) or response.get("schema") != SCHEMA:
            raise ProtocolError(f"{op} response is not a {SCHEMA} message")
        if response.get("ok") is False:
            error = response.get("error")
            if not isinstance(error, dict):
                raise ProtocolError(f"{op} failure has no error object")
            if response.get("id") not in (request_id, None):
                raise ProtocolError(f"failure id {response.get('id')!r} does not answer {request_id}")
            raise RuntimeProtocolError(
                op, str(error.get("code", "unknown")), str(error.get("reason", "")),
                error.get("detail"), error.get("exitCode"), error.get("path"),
            )
        if response.get("id") != request_id or response.get("op") != op:
            raise ProtocolError(f"response {response.get('id')!r}/{response.get('op')!r} does not answer {request_id}/{op}")
        if response.get("ok") is not True or not isinstance(response.get("result"), dict):
            raise ProtocolError(f"{op} success has no result object")
        return response["result"]

    def _handshake(self, expected_sha256: str | None) -> dict[str, Any]:
        result = self.request("handshake", {"protocol": RUNTIME_PROTOCOL})
        if result.get("runtimeProtocol") != RUNTIME_PROTOCOL:
            raise ProtocolError(f"runtime speaks protocol {result.get('runtimeProtocol')!r}, not {RUNTIME_PROTOCOL}")
        if result.get("rendererProtocol") != RENDERER_PROTOCOL:
            raise ProtocolError(f"runtime renderer protocol {result.get('rendererProtocol')!r}, not {RENDERER_PROTOCOL}")
        operations = result.get("operations")
        if not isinstance(operations, list) or "handshake" not in operations:
            raise ProtocolError("handshake result lists no operations")
        if expected_sha256 is not None and result.get("contractSha256") != expected_sha256:
            raise ProtocolError(f"runtime contract {result.get('contractSha256')!r} is not {expected_sha256}")
        return result

    def _require(self, op: str) -> None:
        if op not in self.operations:
            raise RuntimeProtocolError(op, "operation_unavailable", f"the runtime does not offer {op}")

    def _execute(self, op: str, argv: Sequence[str], cwd: str | None, mode: RenderMode | None = None) -> CommandResult:
        self._require(op)
        params: dict[str, Any] = {"argv": [str(a) for a in argv]}
        if cwd is not None:
            params["cwd"] = os.path.abspath(cwd)
        if mode is not None:
            params["mode"] = mode
        result = self.request(op, params)
        if result.get("exitCode") not in (0, 2) or "value" not in result:
            raise ProtocolError(f"{op} result is not a command result")
        return CommandResult(result["value"], int(result["exitCode"]))

    def help(self, argv: Sequence[str] = (), *, cwd: str | None = None) -> CommandResult:
        return self._execute("help", argv, cwd)

    def command(self, argv: Sequence[str], *, cwd: str | None = None) -> CommandResult:
        return self._execute("command", argv, cwd)

    def compile(self, argv: Sequence[str], *, cwd: str | None = None) -> CommandResult:
        return self._execute("compile", argv, cwd)

    def simulate(self, argv: Sequence[str], *, cwd: str | None = None) -> CommandResult:
        return self._execute("simulate", argv, cwd)

    def transfer(self, argv: Sequence[str], *, cwd: str | None = None) -> CommandResult:
        return self._execute("transfer", argv, cwd)

    def evaluate(self, argv: Sequence[str], *, cwd: str | None = None) -> CommandResult:
        return self._execute("evaluate", argv, cwd)

    def render(self, argv: Sequence[str], mode: RenderMode, *, cwd: str | None = None) -> CommandResult:
        return self._execute("render", argv, cwd, mode)

    def preview(self, argv: Sequence[str], mode: RenderMode, *, cwd: str | None = None) -> CommandResult:
        return self._execute("preview", argv, cwd, mode)

    def _env(self, op: str, params: Mapping[str, Any]) -> tuple[str, dict[str, Any], bytes]:
        self._require(op)
        result = self.request(op, params)
        header = result.get("header")
        if not isinstance(result.get("envId"), str) or not isinstance(header, dict) or header.get("ok") is not True:
            raise ProtocolError(f"{op} result is not an env result")
        if "envId" in params and result["envId"] != params["envId"]:
            raise ProtocolError(f"{op} answered handle {result['envId']!r}, not {params['envId']!r}")
        return result["envId"], header, _b64decode(result.get("tailBase64"))

    def env_create(self, argv: Sequence[str], *, cwd: str | None = None) -> tuple[str, dict[str, Any], bytes]:
        """``argv`` is ``env serve``'s without the verb and ``--socket``; returns handle, hello, tail."""
        params: dict[str, Any] = {"argv": [str(a) for a in argv]}
        if cwd is not None:
            params["cwd"] = os.path.abspath(cwd)
        return self._env("env.create", params)

    def env_reset(self, env_id: str, seed: int | float | str | None = None, *, scenario: Mapping[str, Any] | None = None) -> tuple[dict[str, Any], bytes]:
        params: dict[str, Any] = {"envId": env_id, "seed": seed}
        if scenario is not None:
            params["scenario"] = dict(scenario)
        return self._env("env.reset", params)[1:]

    def env_step(self, env_id: str, action: Sequence[float | None] | None) -> tuple[dict[str, Any], bytes]:
        return self._env("env.step", {"envId": env_id, "action": None if action is None else list(action)})[1:]

    def env_observe(self, env_id: str) -> tuple[dict[str, Any], bytes]:
        return self._env("env.observe", {"envId": env_id})[1:]

    def env_checkpoint(self, env_id: str) -> tuple[dict[str, Any], bytes]:
        return self._env("env.checkpoint", {"envId": env_id})[1:]

    def env_restore(self, env_id: str, checkpoint: bytes) -> tuple[dict[str, Any], bytes]:
        return self._env("env.restore", {"envId": env_id, "checkpointBase64": base64.b64encode(checkpoint).decode()})[1:]

    def env_close(self, env_id: str) -> None:
        self._env("env.close", {"envId": env_id})

    def batch_create(self, argv: Sequence[str], count: int, *, shards: int | None = None, cwd: str | None = None) -> RuntimeBatch:
        """``count`` state-only envs of one workspace in the runtime; ``argv`` as for :meth:`env_create`."""
        self._require("env.batch.create")
        count = _index("count", count, 1, MAX_BATCH_ENVS)
        params: dict[str, Any] = {"argv": [str(a) for a in argv], "count": count}
        if shards is not None:
            params["shards"] = _index("shards", shards, 1, count)
        if cwd is not None:
            params["cwd"] = os.path.abspath(cwd)
        result = self.request("env.batch.create", params)
        try:
            shards_out = _index("shards", result["shards"], 1, count)
            shard_of = tuple(_index("shardOf", s, 0, shards_out - 1) for s in result["shardOf"])
            if not isinstance(result["batchId"], str) or not isinstance(result["hello"], dict):
                raise ValueError("batchId must be a string and hello an object")
            batch = RuntimeBatch(result["batchId"], _index("count", result["count"], count, count), shards_out, shard_of, result["hello"])
        except (KeyError, TypeError, ValueError) as error:
            raise ProtocolError(f"env.batch.create result is malformed: {error}") from error
        if len(batch.shard_of) != count or batch.hello.get("ok") is not True:
            raise ProtocolError("env.batch.create answered a different batch than requested")
        return batch

    def _batch(self, op: str, batch_id: str, count: int, params: dict[str, Any], mask: Sequence[bool] | None) -> BatchResults:
        self._require(op)
        if mask is not None:
            if len(mask) != count:
                raise ValueError(f"mask has {len(mask)} entries for {count} envs")
            if not all(isinstance(m, (bool, np.bool_)) for m in mask):
                raise TypeError("mask entries must be booleans")
            mask = [bool(m) for m in mask]
            params["mask"] = mask
        result = self.request(op, {"batchId": batch_id, **params})
        rows_raw, failed = result.get("results"), result.get("failed")
        if result.get("batchId") != batch_id or not isinstance(rows_raw, list) or len(rows_raw) != count or not isinstance(failed, list):
            raise ProtocolError(f"{op} result does not answer batch {batch_id} of {count}")
        rows: list[BatchRow | None] = []
        for k, raw in enumerate(rows_raw):
            selected = mask is None or mask[k]
            if raw is None:
                if selected:
                    raise ProtocolError(f"{op} returned no row for selected env {k}")
                rows.append(None)
                continue
            if not selected or not isinstance(raw, dict) or raw.get("envIndex") != k or not isinstance(raw.get("header"), dict):
                raise ProtocolError(f"{op} row {k} is malformed or unrequested")
            ok, header_ok = raw.get("ok"), raw["header"].get("ok")
            if not isinstance(ok, bool) or ok is not header_ok:
                raise ProtocolError(f"{op} row {k} ok={ok!r} disagrees with its header ok={header_ok!r}")
            rows.append(BatchRow(k, ok, raw["header"], _b64decode(raw.get("tailBase64"))))
        if not all(isinstance(f, int) and not isinstance(f, bool) for f in failed) or sorted(failed) != [r.env_index for r in rows if r is not None and not r.ok]:
            raise ProtocolError(f"{op} failed list {failed} disagrees with its rows")
        return BatchResults(rows, tuple(failed))

    def batch_reset(self, batch: RuntimeBatch, seeds: Sequence[int | float | str | None] | None = None, mask: Sequence[bool] | None = None) -> BatchResults:
        params: dict[str, Any] = {}
        if seeds is not None:
            if len(seeds) != batch.count:
                raise ValueError(f"{len(seeds)} seeds for {batch.count} envs")
            params["seeds"] = list(seeds)
        return self._batch("env.batch.reset", batch.batch_id, batch.count, params, mask)

    def batch_step(self, batch: RuntimeBatch, actions: Sequence[Sequence[float | None] | None], mask: Sequence[bool] | None = None) -> BatchResults:
        """One native row (or ``None`` for authored control) per env; masked envs' entries are ignored."""
        if len(actions) != batch.count:
            raise ValueError(f"{len(actions)} actions for {batch.count} envs")
        return self._batch("env.batch.step", batch.batch_id, batch.count, {"actions": [action_row(a) for a in actions]}, mask)

    def batch_observe(self, batch: RuntimeBatch, mask: Sequence[bool] | None = None) -> BatchResults:
        return self._batch("env.batch.observe", batch.batch_id, batch.count, {}, mask)

    def batch_close(self, batch: RuntimeBatch) -> None:
        self._require("env.batch.close")
        result = self.request("env.batch.close", {"batchId": batch.batch_id})
        if result.get("batchId") != batch.batch_id or result.get("closed") is not True:
            raise ProtocolError(f"env.batch.close did not close {batch.batch_id}")

    def _world(self, op: str, params: dict[str, Any], required: Sequence[str]) -> dict[str, Any]:
        self._require(op)
        result = self.request(op, {k: v for k, v in params.items() if v is not None})
        missing = [k for k in required if k not in result]
        if missing:
            raise ProtocolError(f"{op} result lacks {missing}")
        return result

    def _session(self, op: str, world_id: str, result: dict[str, Any]) -> WorldSession:
        if not isinstance(result["clientId"], str) or not isinstance(result["reconnectToken"], str) or not isinstance(result["host"], bool):
            raise ProtocolError(f"{op} identity fields are malformed")
        return WorldSession(world_id, result["clientId"], result["reconnectToken"], result["host"], int(result["tick"]), result)

    def world_create(self, argv: Sequence[str], *, cwd: str | None = None, pacing: str | None = None, population: Mapping[str, int] | None = None) -> WorldSession:
        """Host a persistent world; the caller becomes its host client."""
        params: dict[str, Any] = {"argv": [str(a) for a in argv], "cwd": None if cwd is None else os.path.abspath(cwd), "pacing": pacing,
                                  "population": None if population is None else dict(population)}
        result = self._world("world.create", params, ("worldId", "clientId", "reconnectToken", "host", "tick", "dtSec", "pacing", "mapId", "mapDigest", "journalPath"))
        if not isinstance(result["worldId"], str) or result["host"] is not True:
            raise ProtocolError("world.create did not return a host session")
        return self._session("world.create", result["worldId"], result)

    def world_join(self, world_id: str, *, client_id: str | None = None, reconnect_token: str | None = None) -> WorldSession:
        """Join a world, or reconnect as ``client_id`` with its ``reconnect_token``."""
        if (client_id is None) != (reconnect_token is None):
            raise ValueError("client_id and reconnect_token go together")
        result = self._world("world.join", {"worldId": world_id, "clientId": client_id, "reconnectToken": reconnect_token}, ("clientId", "reconnectToken", "host", "tick", "dtSec"))
        return self._session("world.join", world_id, result)

    def world_claim(self, world_id: str, actor_id: str, *, release: bool = False) -> dict[str, Any]:
        return self._world("world.claim", {"worldId": world_id, "actorId": actor_id, "action": "release" if release else "claim"}, ("actorId", "owner"))

    def world_command(self, world_id: str, seq: int, command: Mapping[str, Any]) -> dict[str, Any]:
        """One native command at this client's ``seq``; a refused command is an ``outcome`` with ``ok`` false."""
        result = self._world("world.command", {"worldId": world_id, "seq": _index("seq", seq, 0, 2**64 - 1), "command": dict(command)}, ("seq", "tick", "timeSec", "outcome"))
        if result["seq"] != seq or not isinstance(result["outcome"], dict) or not isinstance(result["outcome"].get("ok"), bool):
            raise ProtocolError("world.command acknowledgement does not match the command")
        return result

    def world_subscribe(self, world_id: str, *, rate_hz: int | None = None, capacity: int = 1024) -> Subscription:
        """Push truth deliveries into a local queue of ``capacity`` (drop-oldest, counted)."""
        params = {"worldId": world_id, "rateHz": None if rate_hz is None else _index("rateHz", rate_hz, 1, 50)}
        with self._lock:  # the RPC and the registry hand-off form one transaction
            with self._routing:
                self._subscribing[world_id] = self._subscribing.get(world_id, 0) + 1
            try:
                result = self._world("world.subscribe", params, ("subscriptionId", "dtSec", "rateHz", "capacity", "channels"))
                if not isinstance(result["subscriptionId"], str):
                    raise ProtocolError("world.subscribe returned no subscription id")
                subscription = Subscription(world_id, result["subscriptionId"], result, capacity)
                key = (world_id, subscription.subscription_id)
                with self._routing:
                    held = [m for m in self._pending if m["worldId"] == world_id]
                    self._pending = [m for m in self._pending if m["worldId"] != world_id]
                    for message in held:
                        if message["subscriptionId"] == subscription.subscription_id:
                            subscription._push(message)
                    if subscription.closed_reason is None:
                        self._subscriptions[key] = subscription
                return subscription
            finally:
                with self._routing:
                    self._subscribing[world_id] -= 1
                    if not self._subscribing[world_id]:
                        del self._subscribing[world_id]

    def world_poll(self, world_id: str, subscription_id: str, *, max_frames: int | None = None) -> TruthDelivery:
        params = {"worldId": world_id, "subscriptionId": subscription_id, "maxFrames": None if max_frames is None else _index("maxFrames", max_frames, 1, 256)}
        result = self._world("world.poll", params, ("subscriptionId", "framesBase64", "queued", "dropped"))
        if result["subscriptionId"] != subscription_id:
            raise ProtocolError("world.poll answered another subscription")
        return TruthDelivery(None, split_frames(result["framesBase64"]), int(result["queued"]), int(result["dropped"]))

    def world_unsubscribe(self, subscription: Subscription) -> None:
        with self._lock:
            result = self._world("world.unsubscribe", {"worldId": subscription.world_id, "subscriptionId": subscription.subscription_id}, ("subscriptionId", "closed"))
            if result["closed"] is not True:
                raise ProtocolError("world.unsubscribe did not close the subscription")
            self._forget(subscription.world_id, "unsubscribed", subscription.subscription_id)

    def _forget(self, world_id: str, reason: str, subscription_id: str | None = None) -> None:
        with self._routing:
            keys = [k for k in self._subscriptions if k[0] == world_id and subscription_id in (None, k[1])]
            gone = [self._subscriptions.pop(k) for k in keys]
        for subscription in gone:
            subscription._close(reason)

    def world_snapshot(self, world_id: str) -> dict[str, Any]:
        return self._world("world.snapshot", {"worldId": world_id}, ("tS", "tick", "done", "actors", "signals", "pacing", "lagTicks", "overloadCount", "droppedSimTicks", "haltedReason"))

    def world_checkpoint(self, world_id: str) -> WorldCheckpoint:
        result = self._world("world.checkpoint", {"worldId": world_id}, ("checkpointBase64", "tick", "digest", "baseInputHash"))
        return WorldCheckpoint(_b64decode(result["checkpointBase64"]), int(result["tick"]), str(result["digest"]), str(result["baseInputHash"]))

    def world_restore(self, world_id: str, checkpoint: WorldCheckpoint | bytes) -> dict[str, Any]:
        """Host only; the world restores paused and every subscription to it ends (resubscribe)."""
        data = checkpoint.data if isinstance(checkpoint, WorldCheckpoint) else bytes(checkpoint)
        with self._lock:
            result = self._world("world.restore", {"worldId": world_id, "checkpointBase64": base64.b64encode(data).decode()}, ("tick", "pacing"))
            self._forget(world_id, "restored")
        return result

    def world_time(self, world_id: str, *, pacing: str | None = None, advance_ticks: int | None = None) -> dict[str, Any]:
        params = {"worldId": world_id, "pacing": pacing, "advanceTicks": None if advance_ticks is None else _index("advanceTicks", advance_ticks, 1, 2**32 - 1)}
        return self._world("world.time", params, ("tick", "timeSec", "pacing", "lagTicks", "peakLagTicks", "overloadCount", "droppedSimTicks", "haltedReason", "journalPath"))

    def world_record(self, world_id: str, action: str, *, path: str | None = None, chunk_ticks: int | None = None) -> dict[str, Any]:
        if action == "start" and path is None:
            raise ValueError("world_record start needs a path")
        params = {"worldId": world_id, "action": action, "path": path, "chunkTicks": chunk_ticks}
        return self._world("world.record", params, ("recordingPath", "chunks", "active", "manifestPath", "manifestSha256"))

    def world_close(self, world_id: str) -> None:
        with self._lock:
            result = self._world("world.close", {"worldId": world_id}, ("closed",))
            if result["closed"] is not True:
                raise ProtocolError("world.close did not close the world")
            self._forget(world_id, "world_closed")

    def close(self, timeout: float = 30.0) -> int | None:
        """Close the connection; a spawned runtime exits on EOF and its code is returned."""
        if self._closed:
            return self._process.returncode if self._process else None
        self._closed = True
        if self._closer is not None:
            # Unblock the reader first: closing a buffered socket stream it is blocked in can deadlock.
            _shutdown(self._closer)
            self._thread.join(timeout)
        try:
            self._writer.close()
        except OSError:
            pass
        code = None
        if self._process is not None:
            try:
                code = self._process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self._process.kill()
                code = self._process.wait()
        self._thread.join(timeout)
        if self._closer is not None:
            self._closer.close()
        return code

    def __enter__(self) -> RuntimeClient:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()
