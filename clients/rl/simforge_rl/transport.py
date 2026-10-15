"""Episode transports: env-serve socket and runtime ``env.*`` handles."""

from __future__ import annotations

import os
import socket as _socket
import struct
from typing import TYPE_CHECKING, Any, Mapping, Protocol, Sequence

import numpy as np

from . import wire
from .wire import EnvServeError, ProtocolError

if TYPE_CHECKING:
    from .runtime import RuntimeClient

Response = tuple[dict[str, Any], bytes]


class EnvTransport(Protocol):
    """One episode session: native env-serve headers and binary tails."""

    def hello(self) -> dict[str, Any]: ...
    def reset(self, seed: int | float | str | None = None, *, scenario: Mapping[str, Any] | None = None) -> Response: ...
    def step(self, row: Sequence[float | None] | np.ndarray | None) -> Response: ...
    def observe(self) -> Response: ...
    def checkpoint(self) -> bytes: ...
    def restore(self, checkpoint: bytes) -> Response: ...
    def close(self) -> None: ...


class SocketTransport:
    """One session on ``simforge env serve --socket PATH``; requests are answered in order."""

    def __init__(self, path: str | os.PathLike[str], *, timeout: float | None = None) -> None:
        self.path = os.fspath(path)
        self._sock: _socket.socket | None = _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM)
        self._sock.settimeout(timeout)
        try:
            self._sock.connect(self.path)
        except OSError:
            self._sock.close()
            raise
        self._i = 0
        self._hello: dict[str, Any] | None = None

    def _recv_exact(self, n: int) -> bytes:
        assert self._sock is not None
        chunks = bytearray()
        while len(chunks) < n:
            chunk = self._sock.recv(min(n - len(chunks), 1 << 20))
            if not chunk:
                raise ConnectionError("simforge env serve closed the connection")
            chunks.extend(chunk)
        return bytes(chunks)

    def call(self, op: str, *, tail: bytes = b"", **fields: Any) -> Response:
        """Send ``{op, i, **fields}`` (+ ``tail``); return the response header and tail."""
        if self._sock is None:
            raise ConnectionError("transport is closed")
        self._i += 1
        self._sock.sendall(wire.encode({"op": op, "i": self._i, **fields}, tail))
        (total,) = struct.unpack("<I", self._recv_exact(4))
        if total > wire.MAX_MESSAGE_BYTES:
            raise ProtocolError(f"response of {total} bytes exceeds {wire.MAX_MESSAGE_BYTES}")
        header, payload = wire.decode_body(self._recv_exact(total))
        if header.get("i") != self._i:
            raise ProtocolError(f"response {header.get('i')!r} does not answer request {self._i}")
        if header.get("ok") is not True:
            raise EnvServeError(op, str(header.get("code", "unknown")), str(header.get("reason", "")), header.get("detail"))
        return header, payload

    def hello(self) -> dict[str, Any]:
        if self._hello is None:
            hello = self.call("hello")[0]
            if hello.get("protocol") != wire.PROTOCOL:
                raise ProtocolError(f"{self.path} speaks {hello.get('protocol')!r}, not {wire.PROTOCOL}")
            self._hello = hello
        return self._hello

    def reset(self, seed: int | float | str | None = None, *, scenario: Mapping[str, Any] | None = None) -> Response:
        fields: dict[str, Any] = {"seed": seed}
        if scenario is not None:
            fields["scenario"] = dict(scenario)
        return self.call("reset", **fields)

    def step(self, row: Sequence[float | None] | np.ndarray | None) -> Response:
        return self.call("step", action=wire.action_row(row))

    def observe(self) -> Response:
        return self.call("observe")

    def checkpoint(self) -> bytes:
        header, tail = self.call("checkpoint")
        return wire.array(tail, header["checkpoint"]).tobytes()

    def restore(self, checkpoint: bytes) -> Response:
        ref = {"offset": 0, "length": len(checkpoint), "dtype": "|u1", "shape": [len(checkpoint)]}
        return self.call("restore", checkpoint=ref, tail=checkpoint)

    def close_server(self) -> None:
        """Ask the server to exit (it removes its socket)."""
        self.call("close")

    def close(self) -> None:
        sock, self._sock = self._sock, None
        if sock is not None:
            sock.close()


class RuntimeEnvTransport:
    """One ``env.create`` handle on a :class:`~simforge_rl.runtime.RuntimeClient`."""

    def __init__(self, runtime: RuntimeClient, argv: Sequence[str], *, cwd: str | None = None) -> None:
        self.runtime = runtime
        self.env_id, header, _ = runtime.env_create(argv, cwd=cwd)
        if header.get("protocol") != wire.PROTOCOL:
            self.close()
            raise ProtocolError(f"env.create hello speaks {header.get('protocol')!r}, not {wire.PROTOCOL}")
        self._hello = header

    def hello(self) -> dict[str, Any]:
        return self._hello

    def reset(self, seed: int | float | str | None = None, *, scenario: Mapping[str, Any] | None = None) -> Response:
        return self.runtime.env_reset(self.env_id, seed, scenario=scenario)

    def step(self, row: Sequence[float | None] | np.ndarray | None) -> Response:
        return self.runtime.env_step(self.env_id, wire.action_row(row))

    def observe(self) -> Response:
        return self.runtime.env_observe(self.env_id)

    def checkpoint(self) -> bytes:
        header, tail = self.runtime.env_checkpoint(self.env_id)
        return wire.array(tail, header["checkpoint"]).tobytes()

    def restore(self, checkpoint: bytes) -> Response:
        return self.runtime.env_restore(self.env_id, checkpoint)

    def close(self) -> None:
        env_id, self.env_id = self.env_id, ""
        if env_id and self.runtime.alive:
            self.runtime.env_close(env_id)
