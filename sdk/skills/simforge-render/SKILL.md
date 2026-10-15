---
name: simforge-render
description: Preview and render a SimForge scenario with `simforge preview frame|clip` and `simforge render` in three modes (2d top-down on the CPU, schematic OpenDRIVE view, 3d photoreal with sensors), locally by default or the same command with --cloud and a cost cap; use `simforge rig init` to create a rig, inspect retained output manifests, and diagnose renderer failures.
---

# Preview and render

One command, three modes, two places to run it:

| `--mode` | What | Needs locally |
|---|---|---|
| `2d` | top-down map video following the ego (CPU) | the runtime, the map, `ffmpeg` |
| `schematic` | the flat, unlit OpenDRIVE view Studio draws: one role-coloured shape per actor and the ego's path | the runtime, the map's schematic derivative (installed with `maps pull`), a GPU |
| `3d` | photoreal frames with id/depth/semantic passes, lidar and radar from the rig's sensors | the runtime, the map, actor assets, a GPU |

Without `--cloud` it renders on this machine's runtime; with `--cloud
--max-cost <cents>` the hosted workers render it and `--wait --download`
brings the files back into `--out`. The flags and outputs are the same.

TARGET is the scenario JSON (`simforge-authoring`) or a workspace directory;
with `--cloud` it may also be a hosted scenario id with `--revision`.

## Preview first (cheap)

One frame (PNG) or a few seconds (MP4) of one camera of the rig:

```sh
simforge rig init --preset basic-dash-camera --class car --out rig.json
simforge rig show rig.json                # sources, mounts, facing, field of view; camera names
simforge preview frame s.json --mode 3d --rig rig.json --camera <camera> --at 5 --out cam.png
simforge preview clip s.json --mode schematic --rig rig.json --camera <camera> --at 5 --seconds 3 --out cam.mp4
simforge preview frame s.json --mode 3d --rig rig.json --camera <camera> --at 5 --out cam.png --cloud --max-cost <cents>
```

`--camera` is a source's `outputName`; a name that is not a camera of the rig
fails with `preview_camera_unknown` and lists the cameras it has.

## Render

```sh
simforge render s.json --mode 2d --out out-2d
simforge render s.json --mode schematic --rig rig.json --out out-schematic
simforge render s.json --mode 3d --rig rig.json --preset training --out out-training
simforge render s.json --mode 3d --rig rig.json --preset showcase --out out-showcase
simforge render <scenario-id> --revision <revision-id> --mode 3d --rig rig.json --cloud --dry-run
simforge render s.json --mode 3d --rig rig.json --out out-3d --cloud --max-cost <cents> --wait --download
```

- `--mode` is authoritative; a conflicting engine or preset is refused, never
  rewritten. In `3d`, `--preset training` is the dataset look at a fixed cost
  per frame and `showcase` turns on the expensive lighting for video.
- `--out` must not exist or be empty.
- A render replays the scenario's simulated trace; poses never come from a
  second simulation. To change motion, edit the scenario and `simforge simulate`
  it again (`--out DIR`, the same layout locally and with `--cloud`).
- The rig is `simforge.render-rig/v1`: render-request data, not scenario
  content. Every source mounts on the scenario's ego; a scenario without a
  vehicle is refused (`render_ego_missing`). Without `--rig` the default rig is
  one front camera on the ego.
- More runtime flags (passes, video, texture tier, software adapter): read
  `simforge render --help` once the runtime is installed.

## Rigs

```sh
simforge rig init --preset roof-8cam-6lidar-4radar --class car --actor ego --out rig.json
simforge rig validate rig.json            # exit 2 render_rig_invalid with the failing field
simforge rig show rig.json
```

Rigs work offline, without the runtime or a login, except `validate` and
`show` of a rig with an f-theta lens (`cameraModel`): the runtime checks that
lens, so it must be installed.

## Outputs

`out/render.json` is the summary the command also prints (gates, adapter,
videos, warnings); `results.json` lists every pass with its sha256. Frames and
id/depth/semantic images sit under `out/<sensor>/`, lidar as PLY, radar as
CSV, videos under `video/`. Rig mounts use x forward, y up, z left; lidar
point clouds (PLY) use a different frame: +Z is to the right (CARLA parity).
See the sensor outputs docs. `2d` writes `video/topdown.mp4` and `render.json`.

## Reproducibility

Traces and timelines are byte-identical everywhere. Pixels are identical run
to run on the same GPU model and driver; different GPUs differ in pixels:
compare like with like.

## How fast is this machine?

```sh
simforge bench
```

Renders a pinned pack and reports the Helios Score (1000 = the reference RTX
3080), a sub-score per factor and each scene's per-tick timings; every frame
is checked against the pack's reference. A noisy machine is flagged.

## Reference

<!-- simforge:reference preview frame, preview clip, render, rig init, rig validate, rig show -->
_Generated from `simforge --help` (CLI 0.2.0-rc.4) by scripts/skills/skills.py; do not edit._

#### `simforge preview frame`

One still frame of a scenario at --at seconds (PNG), locally or with --cloud

Usage: `simforge preview frame <target> <extra> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<target>` | required | Scenario JSON file or materialized workspace; identical input for local and cloud |
| `<extra>` | optional |  |
| `--out OUT` | required | Output file (PNG for a frame, MP4 for a clip) |
| `--mode MODE` | one of `2d`, `schematic`, `3d`; default `2d` |  |
| `--rig RIG` |  | Sensor rig JSON (`simforge rig init`). Default: the `basic-dash-camera` rig on the ego |
| `--camera CAMERA` |  | The rig camera to look through |
| `--at AT` | default `0` | Scenario time in seconds |
| `--seconds SECONDS` |  | Clip length in seconds |
| `--preset PRESET` | one of `training`, `showcase`, `schematic` | Render preset |
| `--force` |  | Overwrite an existing output file |
| `--max-cost CENTS` |  | Explicit maximum cloud cost in cents; required for billable cloud jobs |
| `--revision REVISION` |  | Immutable revision for an existing hosted scenario target |
| `--wait` |  | Wait for a submitted cloud job to finish |
| `--download` |  | Download a succeeded cloud job into --out; implies --wait |
| `--map-dir DIR` |  | Render on this native map directory (`.corpus/<map>`) instead of the installed map found by content |

#### `simforge preview clip`

A short clip of a scenario from --at for --seconds (MP4), locally or with --cloud

Usage: `simforge preview clip <target> <extra> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<target>` | required | Scenario JSON file or materialized workspace; identical input for local and cloud |
| `<extra>` | optional |  |
| `--out OUT` | required | Output file (PNG for a frame, MP4 for a clip) |
| `--mode MODE` | one of `2d`, `schematic`, `3d`; default `2d` |  |
| `--rig RIG` |  | Sensor rig JSON (`simforge rig init`). Default: the `basic-dash-camera` rig on the ego |
| `--camera CAMERA` |  | The rig camera to look through |
| `--at AT` | default `0` | Scenario time in seconds |
| `--seconds SECONDS` |  | Clip length in seconds |
| `--preset PRESET` | one of `training`, `showcase`, `schematic` | Render preset |
| `--force` |  | Overwrite an existing output file |
| `--max-cost CENTS` |  | Explicit maximum cloud cost in cents; required for billable cloud jobs |
| `--revision REVISION` |  | Immutable revision for an existing hosted scenario target |
| `--wait` |  | Wait for a submitted cloud job to finish |
| `--download` |  | Download a succeeded cloud job into --out; implies --wait |
| `--map-dir DIR` |  | Render on this native map directory (`.corpus/<map>`) instead of the installed map found by content |

#### `simforge render`

Render the same scenario file locally or with --cloud

Usage: `simforge render <target> <extra> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<target>` | required | Scenario JSON file or materialized workspace; identical input for local and cloud |
| `<extra>` | optional | Runtime-specific options; forwarded without shell interpolation |
| `--out OUT` |  | Output folder; local requires it, cloud waits and downloads when it is given |
| `--mode MODE` | one of `2d`, `schematic`, `3d`; default `2d` |  |
| `--rig RIG` |  | Sensor rig JSON (`simforge rig init`). Default: the `basic-dash-camera` rig on the ego |
| `--preset PRESET` | one of `training`, `showcase`, `schematic` | Render preset |
| `--wait` |  | Wait for a submitted cloud job to finish |
| `--download` |  | Download a succeeded cloud job into --out; implies --wait |
| `--max-cost CENTS` |  | Explicit maximum cloud cost in cents; required for billable cloud jobs |
| `--revision REVISION` |  | Immutable revision for an existing hosted scenario target |
| `--camera CAMERA` | one of `follow-ego`, `overview`; default `follow-ego` | 2d engine: the camera |
| `--north-up` |  | 2d engine, follow-ego: keep north up (looking ahead of the ego) instead of turning the view with it |
| `--debug-signals` |  | 2d engine: also draw signal heads the timeline has no phases for (hollow grey rings) |
| `--no-hud` |  | 2d engine: no HUD (header, legend, clock, speed); the map fills the whole frame |
| `--actor-labels LABELS.JSON` |  | 2d engine: tags to draw next to actors, a JSON object `{actorId: text}` (e.g. a labelling page's candidate numbers). An id that is not an actor of the timeline fails the render |
| `--legibility LEGIBILITY` | one of `standard`, `high`; default `standard` | 2d engine: icon legibility; `high` is for labelling clips (larger props, light people, faint untagged parked vehicles, no edge chevrons or collision marks, non-overlapping tag badges) |
| `--px-per-m PX` |  | 2d engine: a fixed map scale in pixels per metre. Default: 14 to 9 by the ego's speed (follow-ego); the scenario actors fitted, 5 to 12 (overview) |
| `--size WxH` | default `1280x720` | 2d engine: the frame size |
| `--fps FPS` | default `20` | 2d engine: frames per second |
| `--ego ACTOR_ID` |  | 2d engine: the ego actor. Default: the workspace's metric subject |
| `--timeline TIMELINE.JSON` |  | 2d engine: render a render-timeline file instead of a workspace (its map is found by its OpenDRIVE digest) |
| `--clip START:END` |  | 2d engine: the clip, `START:END` seconds of the timeline. Default: the whole timeline |
| `--buildings` |  | 2d engine: draw building footprints when the map has them |
| `--title TEXT` |  | 2d engine: the header's title. Default: the map's name (its id in words) and the scenario's title from the package (else the workspace directory); render.json keeps the ids |
| `--passes PASSES` | one of `rgb`, `id`, `depth`, `semantic`; default `rgb,id,depth,semantic`; repeatable | Camera passes, comma-separated |
| `--set KEY=VALUE` | repeatable | Render-config overrides on the preset (`key=value`, e.g. `aa.mode=fxaa`); repeatable |
| `--textures TEXTURES` | one of `uastc-full`, `bc7-512` | The staged texture tier (photoreal presets). Default: uastc-full |
| `--video VIDEO` | one of `auto`, `required`, `off`; default `auto` | Encode videos with ffmpeg |
| `--vram-budget BYTES` |  | A texture budget in bytes; without it the device capacity is unmeasured and admission is skipped (reported) |
| `--allow-software-adapter` |  | Render on a software (CPU) Vulkan adapter such as lavapipe, explicitly. Slow; pixels differ from a GPU's |
| `--map-dir DIR` |  | Use this native map directory (`.corpus/<map>`) instead of the installed map found by content |
| `--cache-root DIR` |  | Map cache root to search. Default: SIMFORGE_MAPS_CACHE_ROOT, then $XDG_DATA_HOME/simforge/maps |
| `--assets-root DIR` |  | Actor-asset root. Default: SIMFORGE_ACTOR_ASSETS_ROOT, then $XDG_DATA_HOME/simforge/actor-assets |
| `--actor-closure SHA256` |  | The actor closure to bind, instead of the workspace's `catalog.actorClosureDigest` |
| `--actor-layer SHA256` |  | The workspace's private model layer (an actor closure in the same store) for `workspace.*` models, instead of the workspace's `simforge.actorLayer` extension |
| `--allow-map-drift` |  | Render on the installed map release even where its members differ from the workspace's map closure (recorded in the result; the default refuses) |

#### `simforge rig init`

Write a built-in rig fitted to a carrier class's default box as a `simforge.render-rig/v1` file

Usage: `simforge rig init [flags]`

| Argument / flag | | What |
|---|---|---|
| `--preset PRESET` | required; one of `basic-dash-camera`, `tesla-hw3`, `waymo-5th-gen`, `surround-7cam-1lidar`, `roof-8cam-6lidar-4radar`, `alpamayo-2cam`, `alpamayo-4cam`, `alpamayo-6cam`, `alpamayo-6cam-vqa`, `front-4cam-wide-cross-tele`, `drone-inspection-wide`, `drone-inspection-wide-zoom`, `drone-mapping-nadir`, `drone-fpv-forward` | The built-in rig |
| `--class CLASS` | one of `car`, `truck`, `van`, `drone`; default `car` | The carrier class whose default box the mounts are fitted to: car, truck or van for a road rig, drone for a drone rig |
| `--actor ID` | default `ego` | The actor named in the file (every source's `actorId`); a render re-hosts a placeholder on the scenario's ego and refuses a file naming another of its roles |
| `--out FILE` | required | Where to write the rig |
| `--force` |  | Overwrite an existing file |

#### `simforge rig validate`

Check a `simforge.render-rig/v1` file as a render reads it; the installed runtime plans a camera lens (cameraModel). Exit 2 with the finding

Usage: `simforge rig validate <rig.json> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<rig.json>` | required | A `simforge.render-rig/v1` file |

#### `simforge rig show`

What a rig captures: each source's mount, the way it faces and its field of view, and the cameras' horizontal coverage; the installed runtime shows a rig with a camera lens

Usage: `simforge rig show <rig.json> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<rig.json>` | required | A `simforge.render-rig/v1` file |

Every command also takes the global flags `--pretty`, `--json`, `--cloud`, `--workspace`, `--idempotency-key`, `--dry-run`, `--api-url`, `--map-version`, `--help`.

Exit codes: `0` ok; `1` command error: the command could not run; `2` findings: the command ran and found something wrong with its input.
<!-- /simforge:reference -->
