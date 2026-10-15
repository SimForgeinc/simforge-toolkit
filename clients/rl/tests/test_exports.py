from __future__ import annotations

import hashlib
import io
import json
import zipfile
from pathlib import Path

import pytest

from simforge_rl import ExportError, ScenarioSet


def package(files: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for name, data in files.items():
            archive.writestr(name, data)
    return buffer.getvalue()


def write_export(root: Path, rows: list[tuple[str, str, str]]) -> Path:
    """An export folder of (dir, split, map) rows, each with a one-document package."""
    lines = []
    for i, (directory, split, map_name) in enumerate(rows):
        data = package({"manifest.json": b'{"schema":"simforge.scenario-package/v1"}', "document.json": json.dumps({"i": i}).encode()})
        path = root / "scenarios" / directory / "scenario.zip"
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            path.write_bytes(data)
        member = {"path": f"scenarios/{directory}/scenario.zip", "role": "package", "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "size": path.stat().st_size}
        lines.append(json.dumps({"id": f"doc{i}", "dir": directory, "split": split, "map": {"name": map_name, "xodrSha256": "a" * 64}, "members": [member]}))
    text = ("\n".join(lines) + "\n").encode()
    (root / "scenarios.jsonl").write_bytes(text)
    manifest = {"schema": "simforge.dataset-export/v1", "counts": {"scenarios": len(rows)}, "splits": {"counts": {}},
                "files": {"scenarios.jsonl": {"sha256": hashlib.sha256(text).hexdigest(), "size": len(text)}}}
    (root / "manifest.json").write_text(json.dumps(manifest))
    return root


def test_an_export_split_becomes_workspace_tasks(tmp_path: Path) -> None:
    export = write_export(tmp_path / "e", [("b.222", "train", "rfs"), ("a.111", "train", "rfs"), ("a.111", "train", "rfs"), ("c.333", "val", "yale")])
    tasks = ScenarioSet.from_export(export, cache_dir=tmp_path / "cache")
    assert [s.name for s in tasks.scenarios] == ["a.111", "b.222"], "sorted by directory, identical copies once"
    workspace = Path(tasks.scenarios[0].workspace)
    assert json.loads((workspace / "document.json").read_text()) == {"i": 1}
    assert ScenarioSet.from_export(export, cache_dir=tmp_path / "cache") == tasks, "unpacked once per package"
    assert [s.name for s in ScenarioSet.from_export(export, split=None, map_name="yale", cache_dir=tmp_path / "cache").scenarios] == ["c.333"]
    with pytest.raises(ExportError) as empty:
        ScenarioSet.from_export(export, split="test", cache_dir=tmp_path / "cache")
    assert empty.value.code == "export_split_empty"


def test_a_changed_file_or_unknown_source_is_refused(tmp_path: Path) -> None:
    export = write_export(tmp_path / "e", [("a.111", "train", "rfs")])
    (export / "scenarios" / "a.111" / "scenario.zip").write_bytes(package({"document.json": b"{}"}))
    with pytest.raises(ExportError) as changed:
        ScenarioSet.from_export(export, cache_dir=tmp_path / "cache")
    assert changed.value.code == "export_digest_mismatch"
    (export / "scenarios.jsonl").write_text("{}\n")
    with pytest.raises(ExportError) as rows:
        ScenarioSet.from_export(export, cache_dir=tmp_path / "cache")
    assert rows.value.code == "export_digest_mismatch"
    with pytest.raises(ExportError) as missing:
        ScenarioSet.from_export(tmp_path / "nowhere", cache_dir=tmp_path / "cache")
    assert missing.value.code == "export_file_missing"


def test_a_package_member_outside_the_package_is_refused(tmp_path: Path) -> None:
    export = write_export(tmp_path / "e", [("a.111", "train", "rfs")])
    zipped = export / "scenarios" / "a.111" / "scenario.zip"
    zipped.write_bytes(package({"../escape.json": b"{}"}))
    rows = [json.loads(line) for line in (export / "scenarios.jsonl").read_text().splitlines()]
    rows[0]["members"][0]["sha256"] = hashlib.sha256(zipped.read_bytes()).hexdigest()
    text = ("\n".join(json.dumps(r) for r in rows) + "\n").encode()
    (export / "scenarios.jsonl").write_bytes(text)
    manifest = json.loads((export / "manifest.json").read_text())
    manifest["files"]["scenarios.jsonl"]["sha256"] = hashlib.sha256(text).hexdigest()
    (export / "manifest.json").write_text(json.dumps(manifest))
    with pytest.raises(ExportError) as unsafe:
        ScenarioSet.from_export(export, cache_dir=tmp_path / "cache")
    assert unsafe.value.code == "package_member_unsafe"
    assert not (tmp_path / "escape.json").exists()


def write_public(root: Path, rows: list[tuple[str, str, str]]) -> Path:
    """A ``simforge.public-dataset/v1`` folder of (id, split, map slug) rows, each a document and one recording."""
    lines, sums = [], []
    for scenario_id, split, slug in rows:
        files = []
        for role, rel, data in (("document", "document.json", json.dumps({"id": scenario_id}).encode()),
                                ("recording", f"recordings/{'c' * 64}.rec.json.gz", b"recording-bytes"),
                                ("trace", "trace.json.gz", b"trace")):
            path = root / "scenarios" / slug / scenario_id / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            files.append({"path": str(path.relative_to(root)), "role": role, "sha256": hashlib.sha256(data).hexdigest(), "size": len(data)})
            sums.append(f"{files[-1]['sha256']}  {files[-1]['path']}")
        lines.append(json.dumps({"id": scenario_id, "split": split, "map": {"slug": slug, "version": "v9", "xodrSha256": "b" * 64}, "files": files}))
    text = ("\n".join(lines) + "\n").encode()
    (root / "scenarios.jsonl").write_bytes(text)
    sums.append(f"{hashlib.sha256(text).hexdigest()}  scenarios.jsonl")
    sums_bytes = ("\n".join(sorted(sums, key=lambda s: s.split('  ')[1])) + "\n").encode()
    (root / "SHA256SUMS").write_bytes(sums_bytes)
    manifest = {"schema": "simforge.public-dataset/v1", "counts": {"scenarios": len(rows), "bySplit": {}},
                "files": {"SHA256SUMS": {"sha256": hashlib.sha256(sums_bytes).hexdigest(), "size": len(sums_bytes)}}}
    (root / "manifest.json").write_text(json.dumps(manifest))
    return root


def test_the_public_dataset_becomes_workspaces_on_its_maps(tmp_path: Path) -> None:
    public = write_public(tmp_path / "p", [("f" * 64, "train", "yale-street"), ("e" * 64, "val", "yale-street")])
    tasks = ScenarioSet.from_export(public, cache_dir=tmp_path / "cache")
    assert [s.name for s in tasks.scenarios] == ["f" * 64]
    workspace = Path(tasks.scenarios[0].workspace)
    manifest = json.loads((workspace / "manifest.json").read_text())
    assert manifest["map"] == {"sourceMapId": "yale-street", "xodrSha256": "b" * 64}
    assert manifest["recordings"] == [{"sha256": "c" * 64, "path": f"recordings/{'c' * 64}.rec.json.gz"}]
    assert json.loads((workspace / "document.json").read_text()) == {"id": "f" * 64}
    assert not (workspace / "trace.json.gz").exists(), "the runtime compiles the document; the published trace is not an input"
    (public / "SHA256SUMS").write_text("tampered\n")
    with pytest.raises(ExportError) as sums:
        ScenarioSet.from_export(public, cache_dir=tmp_path / "cache2")
    assert sums.value.code == "export_digest_mismatch"
