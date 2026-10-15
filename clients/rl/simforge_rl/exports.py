"""SimForge dataset exports as scenario-set tasks.

Two folder formats, both with ``manifest.json`` and ``scenarios.jsonl`` (one row per scenario,
with its split, map and files):

- ``simforge.dataset-export/v1``, a cloud export (``POST /api/v1/exports``; an export id is
  downloaded with ``simforge datasets pull``, which checks every file): each scenario is a
  ``scenario.zip`` package, unpacked into a workspace whose episode is the input its
  simulation ran.
- ``simforge.public-dataset/v1``, the public SimForge dataset: each scenario is a
  ``document.json`` and the recordings it pins, laid out as a workspace the runtime compiles.

Either way an episode runs on the installed map whose OpenDRIVE digest the scenario names.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import zipfile
from pathlib import Path
from typing import Any

from .server import child_env, find_binary

EXPORT_SCHEMA = "simforge.dataset-export/v1"
PUBLIC_SCHEMA = "simforge.public-dataset/v1"
EXPORT_ID = re.compile(r"^run_[0-9A-Za-z]+$")


class ExportError(ValueError):
    """A named refusal (the codes of ``sdk/docs/engineering/dataset-export.md`` section 9)."""

    def __init__(self, code: str, reason: str) -> None:
        super().__init__(f"{code}: {reason}")
        self.code = code
        self.reason = reason


def cache_root() -> Path:
    """``$XDG_CACHE_HOME/simforge-rl``, else ``~/.cache/simforge-rl``."""
    base = os.environ.get("XDG_CACHE_HOME")
    return (Path(base) if base and os.path.isabs(base) else Path.home() / ".cache") / "simforge-rl"


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pull_export(export_id: str, out: str | os.PathLike[str], *, binary: str | os.PathLike[str] | None = None) -> Path:
    """Download cloud export ``export_id`` as a folder with ``simforge datasets pull`` (it resumes and checks every file)."""
    if not EXPORT_ID.match(export_id):
        raise ExportError("export_id_invalid", f"{export_id!r} is not an export id (run_...)")
    folder = Path(out)
    completed = subprocess.run(
        [find_binary(binary), "datasets", "pull", export_id, "--out", os.fspath(folder), "--folder", "--json"],
        capture_output=True,
        text=True,
        env=child_env(),
    )
    if completed.returncode != 0:
        lines = completed.stderr.strip().splitlines()
        try:
            error: Any = json.loads(lines[-1]) if lines else {}
        except json.JSONDecodeError:
            error = {}
        if isinstance(error, dict) and "code" in error:
            raise ExportError(str(error["code"]), str(error.get("reason", "")))
        raise ExportError("download_failed", f"simforge datasets pull exited {completed.returncode}: {completed.stderr.strip()}")
    return folder


def read_export(root: str | os.PathLike[str]) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    """The manifest and rows of an export folder; ``scenarios.jsonl`` must be the file the manifest names."""
    folder = Path(root)
    manifest_path, rows_path = folder / "manifest.json", folder / "scenarios.jsonl"
    for path in (manifest_path, rows_path):
        if not path.is_file():
            raise ExportError("export_file_missing", f"{path} is missing: {folder} is not a downloaded export folder")
    manifest = json.loads(manifest_path.read_bytes())
    schema = manifest.get("schema") if isinstance(manifest, dict) else None
    if schema == EXPORT_SCHEMA:
        want = manifest["files"]["scenarios.jsonl"]["sha256"]
    elif schema == PUBLIC_SCHEMA:
        sums = folder / "SHA256SUMS"
        if not sums.is_file() or _sha256(sums) != manifest["files"]["SHA256SUMS"]["sha256"]:
            raise ExportError("export_digest_mismatch", f"{sums} is missing or not the SHA256SUMS the manifest names")
        listed = dict(reversed(line.split("  ", 1)) for line in sums.read_text().splitlines() if line)
        if "scenarios.jsonl" not in listed:
            raise ExportError("export_file_missing", "SHA256SUMS does not list scenarios.jsonl")
        want = listed["scenarios.jsonl"]
    else:
        raise ExportError("export_schema_unsupported", f"{manifest_path} is neither {EXPORT_SCHEMA} nor {PUBLIC_SCHEMA}")
    if _sha256(rows_path) != want:
        raise ExportError("export_digest_mismatch", f"{rows_path} is not the scenarios.jsonl the manifest names (sha256 {want})")
    rows = [json.loads(line) for line in rows_path.read_text().splitlines() if line.strip()]
    if len(rows) != manifest["counts"]["scenarios"]:
        raise ExportError("export_rows_count", f"{len(rows)} rows, the manifest counts {manifest['counts']['scenarios']}")
    return manifest, rows


def _checked(path: Path, sha256: str) -> Path:
    if not path.is_file():
        raise ExportError("export_file_missing", f"{path} is missing")
    if _sha256(path) != sha256:
        raise ExportError("export_digest_mismatch", f"{path} is not the file its row names (sha256 {sha256})")
    return path


def _public_workspace(folder: Path, row: dict[str, Any], dest: Path) -> Path:
    """A public scenario's document and recordings as a package workspace on its map, once per scenario."""
    done = dest / ".simforge-rl-package"
    key = row["id"]
    if done.is_file() and done.read_text() == key:
        return dest
    partial = dest.with_name(dest.name + ".partial")
    shutil.rmtree(partial, ignore_errors=True)
    recordings = []
    for entry in row["files"]:
        if entry["role"] not in ("document", "recording"):
            continue
        source = _checked(folder / entry["path"], entry["sha256"])
        name = "document.json" if entry["role"] == "document" else f"recordings/{source.name}"
        (partial / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, partial / name)
        if entry["role"] == "recording":
            recordings.append({"sha256": source.name.split(".", 1)[0], "path": name})
    if not (partial / "document.json").is_file():
        raise ExportError("export_row_unlisted", f"scenario {key} lists no document")
    manifest = {"schema": "simforge.scenario-package/v1", "map": {"xodrSha256": row["map"]["xodrSha256"], "sourceMapId": row["map"]["slug"]},
                "timelines": [], "recordings": recordings}
    (partial / "manifest.json").write_text(json.dumps(manifest, sort_keys=True))
    done = partial / ".simforge-rl-package"
    done.write_text(key)
    shutil.rmtree(dest, ignore_errors=True)
    partial.rename(dest)
    return dest


def _unpack(package: Path, sha256: str, dest: Path) -> Path:
    """``scenario.zip`` as a workspace directory, once per package digest."""
    done = dest / ".simforge-rl-package"
    if done.is_file() and done.read_text() == sha256:
        return dest
    if _sha256(package) != sha256:
        raise ExportError("export_digest_mismatch", f"{package} is not the package its row names (sha256 {sha256})")
    partial = dest.with_name(dest.name + ".partial")
    shutil.rmtree(partial, ignore_errors=True)
    with zipfile.ZipFile(package) as archive:
        for member in archive.infolist():
            name = Path(member.filename)
            if name.is_absolute() or ".." in name.parts:
                raise ExportError("package_member_unsafe", f"{package} has member {member.filename!r} outside the package")
        archive.extractall(partial)
    (partial / ".simforge-rl-package").write_text(sha256)
    shutil.rmtree(dest, ignore_errors=True)
    partial.rename(dest)
    return dest


def export_scenarios(
    source: str | os.PathLike[str],
    *,
    split: str | None = "train",
    map_name: str | None = None,
    cache_dir: str | os.PathLike[str] | None = None,
    binary: str | os.PathLike[str] | None = None,
) -> list[Any]:
    """One workspace :class:`~simforge_rl.scenarios.Scenario` per package of the export's ``split`` (all with ``None``).

    ``source`` is an export folder, or a cloud export id pulled into the cache first. Packages
    unpack under ``cache_dir`` (default :func:`cache_root`), keyed by export and package.
    """
    from .scenarios import Scenario

    cache = Path(cache_dir) if cache_dir is not None else cache_root()
    folder = Path(source)
    if not folder.exists():
        if not EXPORT_ID.match(os.fspath(source)):
            raise ExportError("export_file_missing", f"{source} is neither an export folder nor an export id (run_...)")
        folder = pull_export(os.fspath(source), cache / "exports" / os.fspath(source), binary=binary)
    manifest, rows = read_export(folder)
    export_id = hashlib.sha256((folder / "manifest.json").read_bytes()).hexdigest()
    public = manifest["schema"] == PUBLIC_SCHEMA
    map_of = (lambda r: r["map"]["slug"]) if public else (lambda r: r["map"]["name"])
    chosen = [r for r in rows if (split is None or r["split"] == split) and (map_name is None or map_of(r) == map_name)]
    if not chosen:
        splits = manifest["counts"]["bySplit"] if public else manifest["splits"]["counts"]
        raise ExportError("export_split_empty", f"no scenarios of split {split!r} on map {map_name!r}; the export's splits: {splits}")
    root = cache / "workspaces" / export_id[:16]
    scenarios, seen = [], set()
    for row in sorted(chosen, key=lambda r: r["id"] if public else r["dir"]):
        name = row["id"] if public else row["dir"]
        if name in seen:
            continue
        seen.add(name)
        if public:
            workspace = _public_workspace(folder, row, root / name)
        else:
            member = next((m for m in row["members"] if m["role"] == "package"), None)
            if member is None:
                raise ExportError("export_row_unlisted", f"scenario {row['id']} lists no package")
            package = folder / member["path"]
            if not package.is_file():
                raise ExportError("export_file_missing", f"{package} is missing")
            workspace = _unpack(package, member["sha256"], root / name)
        scenarios.append(Scenario(name, workspace=os.fspath(workspace)))
    return scenarios
