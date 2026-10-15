"""Build the bundled per-map task sets: every template on the best sites of each released map, kept only if it runs.

    python clients/rl/scripts/build_task_sets.py --maps-root ROOT --map richmond-field-station@v9 ... \\
        [--sites 3] [--min-steps 30] [--jobs 6] [--report REPORT.json]

ROOT holds installed maps (``ROOT/dev-assets/<name>`` or ``ROOT/.corpus/<name>``, the layout
``simforge maps pull`` writes). For each map and each ``simforge_rl/tasks/templates`` template,
``simforge sites match`` names the best ``--sites`` sites; each site is compiled and driven for two
seeds of the scenario's authored ego control through ``simforge env serve`` (the installed
runtime). A site is kept when it compiles and the authored episode lasts at least ``--min-steps``
decisions. Each map gets ``simforge_rl/tasks/<name>.json``, pinned to its OpenDRIVE digest, with
every template weighted equally across its kept sites.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

from simforge_rl import EnvServeError, EnvServer, MapPin, Scenario, ScenarioSet, SimForgeEnv, SocketTransport, template_workspace
from simforge_rl.scenarios import TASKS
from simforge_rl.server import child_env, find_binary

SEEDS = (1001, 1002)


def map_dir(root: Path, name: str) -> Path:
    for profile in ("dev-assets", ".corpus"):
        if (root / profile / name / "map.xodr").is_file():
            return root / profile / name
    raise SystemExit(f"{name} is not installed under {root}")


def sites(root: Path, name: str, template: Path, count: int) -> list[str]:
    completed = subprocess.run(
        [find_binary(), "sites", "match", "--map", name, "--max-sites", str(count), os.fspath(template), "--json"],
        capture_output=True, text=True, env=child_env({"SIMFORGE_MAPS_CACHE_ROOT": os.fspath(root)}),
    )
    if completed.returncode not in (0, 2):
        raise SystemExit(f"sites match {name} {template.name}: {completed.stderr.strip()}")
    document = json.loads(completed.stdout or "{}")
    return [s["siteId"] for m in document.get("maps", []) for s in m.get("sites", [])][:count]


def drive(directory: Path, name: str, template: Path, site: str, work: Path) -> dict[str, Any]:
    """Compile ``template`` at ``site`` and run the authored ego control for each seed."""
    tag = hashlib.sha256(f"{name}/{template.name}/{site}".encode()).hexdigest()[:12]
    ws = template_workspace(template, directory, work / tag, site=site)
    row: dict[str, Any] = {"map": name, "template": template.name, "site": site}
    try:
        with EnvServer(ws, work / f"{tag}.sock", "--no-sensors", "--map-dir", os.fspath(directory), "--max-sessions", "1") as server:
            env = SimForgeEnv(SocketTransport(server.socket))
            episodes = []
            for seed in SEEDS:
                env.reset(options={"episode_seed": seed})
                total, steps, done = 0.0, 0, False
                while not done:
                    _, reward, terminated, truncated, _ = env.step(None)
                    total, steps, done = total + reward, steps + 1, terminated or truncated
                episodes.append({"seed": seed, "steps": steps, "return": round(total, 4), "terminated": bool(terminated)})
            env.close()
            row.update(ok=True, inputDigest=server.ready["input"]["digest"], episodes=episodes)
    except EnvServeError as error:
        row.update(ok=False, code=error.code, reason=error.reason[:400])
    return row


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--maps-root", required=True, type=Path)
    parser.add_argument("--map", action="append", required=True, metavar="NAME@VERSION")
    parser.add_argument("--sites", type=int, default=3)
    parser.add_argument("--min-steps", type=int, default=30)
    parser.add_argument("--jobs", type=int, default=6)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    templates = sorted((TASKS / "templates").glob("*.template.json"))
    report: dict[str, Any] = {"schema": "simforge-rl.task-set-build/v1", "seeds": list(SEEDS), "minSteps": args.min_steps, "maps": []}
    with tempfile.TemporaryDirectory(prefix="sfrl-sets-", dir="/tmp") as temporary, ThreadPoolExecutor(args.jobs) as pool:
        for spec in args.map:
            name, _, version = spec.partition("@")
            directory = map_dir(args.maps_root, name)
            xodr = hashlib.sha256((directory / "map.xodr").read_bytes()).hexdigest()
            jobs = [(t, s) for t in templates for s in sites(args.maps_root, name, t, args.sites)]
            rows = list(pool.map(lambda job: drive(directory, name, job[0], job[1], Path(temporary)), jobs))
            kept = [r for r in rows if r["ok"] and min(e["steps"] for e in r["episodes"]) >= args.min_steps]
            per_template: dict[str, int] = {}
            for r in kept:
                per_template[r["template"]] = per_template.get(r["template"], 0) + 1
            scenarios = [
                Scenario(f"{r['template'].removesuffix('.template.json')}@{r['site'][:8]}", template=f"templates/{r['template']}",
                         site=r["site"], weight=round(1.0 / per_template[r["template"]], 6))
                for r in kept
            ]
            if scenarios:
                ScenarioSet.of(scenarios, MapPin(name, version, xodr)).save(TASKS / f"{name}.json")
            report["maps"].append({"name": name, "version": version, "xodrSha256": xodr, "templates": len(per_template), "scenarios": len(kept), "rows": rows})
            print(json.dumps({"map": spec, "templates": len(per_template), "kept": len(kept), "tried": len(rows)}), flush=True)
    if args.report is not None:
        args.report.write_text(json.dumps(report, indent=1, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
