"""Launch ``simforge env serve`` and lay out template workspaces it can serve."""

from __future__ import annotations

import gzip
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any, Mapping

from .wire import PROTOCOL, EnvServeError

READY_SCHEMA = "simforge.env-serve-ready/v1"
CREDENTIAL_ENV = frozenset({"SIMFORGE_API_KEY", "SIMFORGE_TOKEN", "SIMFORGE_ACCESS_TOKEN", "SIMFORGE_REFRESH_TOKEN", "SIMFORGE_MAPS_REGISTRY_TOKEN", "SIMFORGE_CREDENTIAL_STORE"})


def child_env(extra: Mapping[str, str] | None = None) -> dict[str, str]:
    """The local runtime's environment: ours minus cloud credentials, plus explicit ``extra``."""
    return {**{k: v for k, v in os.environ.items() if k not in CREDENTIAL_ENV}, **(extra or {})}


def find_binary(binary: str | os.PathLike[str] | None = None) -> str:
    """``binary``, else ``$SIMFORGE_BIN``, else ``simforge`` on PATH."""
    exe = os.fspath(binary) if binary is not None else os.environ.get("SIMFORGE_BIN") or shutil.which("simforge")
    if not exe:
        raise FileNotFoundError("no simforge binary: pass binary=, set SIMFORGE_BIN, or put simforge on PATH")
    return exe


def runtime_root() -> Path:
    """Where ``simforge runtime install`` puts the runtime: ``$SIMFORGE_RUNTIME_ROOT``, else the data home's ``simforge/runtime``."""
    root = os.environ.get("SIMFORGE_RUNTIME_ROOT")
    if root:
        return Path(root).absolute()
    data = os.environ.get("XDG_DATA_HOME")
    base = Path(data) if data and os.path.isabs(data) else Path.home() / ".local" / "share"
    return base / "simforge" / "runtime"


def find_runtime(binary: str | os.PathLike[str] | None = None) -> str:
    """``binary``, else ``$SIMFORGE_RUNTIME_BIN``, else the installed runtime, else ``simforge-runtime`` on PATH."""
    if binary is not None:
        return os.fspath(binary)
    if os.environ.get("SIMFORGE_RUNTIME_BIN"):
        return os.environ["SIMFORGE_RUNTIME_BIN"]
    installed = runtime_root() / "bin" / "simforge-runtime"
    if installed.is_file():
        return os.fspath(installed)
    exe = shutil.which("simforge-runtime")
    if not exe:
        raise FileNotFoundError(
            f"no simforge-runtime: install it with `simforge runtime install` (expected {installed}), set SIMFORGE_RUNTIME_BIN, or pass binary="
        )
    return exe


def runtime_env(exe: str, extra: Mapping[str, str] | None = None) -> dict[str, str]:
    """The runtime's environment as the CLI starts it: no cloud credentials, plus its install's sky, actors and libraries."""
    env = child_env(extra)
    root = Path(exe).absolute().parent.parent
    for key, member in (("SIMFORGE_SKY_ASSETS", "share/sky"), ("SIMFORGE_ACTOR_ASSETS_ROOT", "share/actors")):
        if (root / member).is_dir() and key not in env:
            env[key] = os.fspath(root / member)
    if (root / "lib").is_dir():
        env["LD_LIBRARY_PATH"] = os.pathsep.join(filter(None, [os.fspath(root / "lib"), env.get("LD_LIBRARY_PATH")]))
    return env


class EnvServer:
    """A running ``simforge env serve WORKSPACE --socket SOCKET``; stops on :meth:`close`."""

    def __init__(
        self,
        workspace: str | os.PathLike[str],
        socket: str | os.PathLike[str],
        *args: str,
        binary: str | os.PathLike[str] | None = None,
        env: Mapping[str, str] | None = None,
    ) -> None:
        self.workspace = Path(workspace)
        self.socket = Path(os.path.abspath(socket))
        self.binary = find_binary(binary)
        self.args = tuple(args)
        self.process = subprocess.Popen(
            [self.binary, "env", "serve", os.fspath(workspace), "--socket", os.fspath(self.socket), *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=child_env(env),
        )
        assert self.process.stdout is not None
        line = self.process.stdout.readline()
        if not line:
            stderr = self.process.stderr.read().decode(errors="replace") if self.process.stderr else ""
            self.process.wait()
            try:
                refusal = json.loads(stderr.strip().splitlines()[-1]) if stderr.strip() else {}
            except json.JSONDecodeError:
                refusal = {}
            if isinstance(refusal, dict) and "code" in refusal:
                raise EnvServeError("serve", str(refusal["code"]), str(refusal.get("reason", "")), refusal.get("detail"), self.process.returncode)
            raise RuntimeError(f"simforge env serve exited ({self.process.returncode}) before listening: {stderr.strip()}")
        ready: dict[str, Any] = json.loads(line)
        if ready.get("schema") != READY_SCHEMA or ready.get("protocol") != PROTOCOL:
            self.close()
            raise RuntimeError(f"unexpected ready line {ready!r}")
        if Path(ready["socket"]) != self.socket:
            self.close()
            raise RuntimeError(f"server listens on {ready['socket']}, not {self.socket}")
        self.ready = ready

    def close(self, timeout: float = 30.0) -> int:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        for stream in (self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()
        return int(self.process.returncode)

    def __enter__(self) -> EnvServer:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


def _xodr_sha256(map_dir: Path) -> str:
    plain, packed = map_dir / "map.xodr", map_dir / "map.xodr.gz"
    if plain.exists():
        return hashlib.sha256(plain.read_bytes()).hexdigest()
    if packed.exists():
        return hashlib.sha256(gzip.decompress(packed.read_bytes())).hexdigest()
    raise FileNotFoundError(f"{map_dir} has no map.xodr or map.xodr.gz")


def template_workspace(
    template: str | os.PathLike[str],
    map_dir: str | os.PathLike[str] | None,
    out: str | os.PathLike[str],
    *,
    map_id: str | None = None,
    xodr_sha256: str | None = None,
    site: str | None = None,
    actor_closure: str | None = None,
) -> Path:
    """A package workspace without a simulation: ``env serve`` compiles ``template`` on a map.

    The map is ``map_dir``, or with ``map_dir=None`` the installed map ``map_id`` whose
    OpenDRIVE has ``xodr_sha256`` (the runtime finds it among installed maps). ``site`` pins
    the template to that site of the map (``anchor.pin``), as ``simforge instantiate`` does;
    the map's own directory name is its id, so pin only on a map named by its id.
    """
    if map_dir is not None:
        map_path = Path(map_dir)
        xodr, source = _xodr_sha256(map_path), map_id or map_path.name
    elif map_id and xodr_sha256:
        xodr, source = xodr_sha256, map_id
    else:
        raise ValueError("template_workspace needs map_dir, or map_id and xodr_sha256 of an installed map")
    out_dir = Path(out)
    out_dir.mkdir(parents=True, exist_ok=True)
    document = Path(template).read_bytes()
    parsed = json.loads(document)
    if site is not None:
        parsed["anchor"]["pin"] = {"mapId": source, "siteId": site}
        document = json.dumps(parsed).encode()
    (out_dir / "document.json").write_bytes(document)
    manifest: dict[str, Any] = {
        "schema": "simforge.scenario-package/v1",
        "map": {"xodrSha256": xodr, "sourceMapId": source},
        "timelines": [],
    }
    if actor_closure is not None:
        manifest["catalog"] = {"actorClosureDigest": actor_closure}
    (out_dir / "manifest.json").write_text(json.dumps(manifest, sort_keys=True))
    return out_dir
