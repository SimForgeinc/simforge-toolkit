from __future__ import annotations

import json
import re
from collections import defaultdict
from pathlib import Path

import pytest

from simforge_rl import MapPin, Scenario, ScenarioSet, bundled_sets, template_workspace
from simforge_rl.scenarios import TASKS, _workspaces

ROOT = Path(__file__).resolve().parents[3]
RELEASED = {"belmont-research-center", "di-rosa-sf", "easterbrook-discovery-school", "el-camino-road", "richmond-field-station", "san-ramon-phase-1", "yale-street"}


def test_every_bundled_set_is_a_released_map_pinned_to_its_opendrive() -> None:
    assert set(bundled_sets()) <= RELEASED and len(bundled_sets()) >= 5
    for name in bundled_sets():
        tasks = ScenarioSet.bundled(name)
        assert tasks.map is not None and tasks.map.name == name and re.fullmatch(r"[0-9a-f]{64}", tasks.map.xodr_sha256)
        per_template: dict[str, float] = defaultdict(float)
        for s in tasks.scenarios:
            assert s.template is not None and Path(s.template).is_file() and s.site
            per_template[Path(s.template).name] += s.weight
        assert all(w == pytest.approx(1.0, abs=1e-5) for w in per_template.values()), "every template weighs the same"
    with pytest.raises(KeyError, match="bundled"):
        ScenarioSet.bundled("san-francisco-phase-1")


def test_task_templates_use_only_public_actors_and_match_their_examples() -> None:
    """Templates copied from sdk/examples stay byte-identical to their source."""
    examples = {p.name: p for p in (ROOT / "sdk/examples").rglob("*.template.json")}
    for template in sorted((TASKS / "templates").glob("*.template.json")):
        if template.name in examples:
            assert template.read_bytes() == examples[template.name].read_bytes(), template.name
        ids = set(re.findall(r'"catalogId":\s*"([^"]+)"', template.read_text()))
        assert all(i.split(".")[0] in {"vehicle", "pedestrian", "carla", "construction"} for i in ids), (template.name, ids)


def test_a_site_pins_the_template_and_a_map_pin_targets_the_installed_map(tmp_path: Path) -> None:
    template = tmp_path / "t.json"
    template.write_text(json.dumps({"scenarioVersion": 2, "anchor": {"id": "a"}}))
    (tmp_path / "map").mkdir()
    (tmp_path / "map" / "map.xodr").write_text("<OpenDRIVE/>")
    ws = template_workspace(template, tmp_path / "map", tmp_path / "w1", map_id="m", site="abc")
    assert json.loads((ws / "document.json").read_text())["anchor"]["pin"] == {"mapId": "m", "siteId": "abc"}
    pinned = ScenarioSet.of([Scenario("t", template=str(template), site="abc")], MapPin("m", "v3", "f" * 64))
    ws2 = Path(_workspaces(pinned, tmp_path / "ws", None)["t"])
    assert json.loads((ws2 / "manifest.json").read_text())["map"] == {"sourceMapId": "m", "xodrSha256": "f" * 64}
    with pytest.raises(ValueError, match="map_dir"):
        _workspaces(ScenarioSet.of([Scenario("t", template=str(template))]), tmp_path / "ws3", None)
    with pytest.raises(ValueError, match="verified on m v3"):
        _workspaces(pinned, tmp_path / "ws4", str(tmp_path / "map"))
    with pytest.raises(ValueError, match="only a template"):
        Scenario("w", workspace="ws", site="abc")


def test_set_files_keep_the_map_pin_and_refuse_unknown_fields(tmp_path: Path) -> None:
    tasks = ScenarioSet.of([Scenario("a", template="t.json", site="s1", weight=0.5)], MapPin("m", "v1", "e" * 64))
    tasks.save(tmp_path / "set.json")
    assert ScenarioSet.load(tmp_path / "set.json") == ScenarioSet.of(
        [Scenario("a", template=str(tmp_path / "t.json"), site="s1", weight=0.5)], MapPin("m", "v1", "e" * 64))
    document = json.loads((tmp_path / "set.json").read_text())
    (tmp_path / "bad.json").write_text(json.dumps({**document, "maps": []}))
    with pytest.raises(ValueError, match="maps"):
        ScenarioSet.load(tmp_path / "bad.json")
    (tmp_path / "bad.json").write_text(json.dumps({**document, "map": {"name": "m"}}))
    with pytest.raises(ValueError, match="xodrSha256"):
        ScenarioSet.load(tmp_path / "bad.json")
