---
name: simforge-maps-assets
description: Download SimForge Worlds (HD maps with their digital twins; the CLI calls them maps) your signed-in account is entitled to (`simforge maps pull name@version`), the actor-asset closure (`simforge assets pull`), manage cache locations, check custom vehicle models, and respect the models' licences (CC BY 4.0 attribution). Use when a local command says a map or actor closure is missing, when a map download is refused, or when asked about asset licences.
---

# SimForge Worlds (maps) and actor assets

Both are immutable and content-addressed: every blob is verified by sha256 on
download, and a scenario pins exactly which map release and actor closure it
needs. Local commands find them by digest, never by name; a missing one is an
error naming the pull command, and nothing is substituted. Hosted runs
(`--cloud`) resolve maps on SimForge's workers and need no local download.

## Maps (sign-in and entitlement)

```sh
simforge login
simforge maps pull easterbrook-discovery-school@v9 --accept-license
simforge maps pull easterbrook-discovery-school@v9 --cache-root /data/simforge/maps
```

`maps pull` and `assets pull` are local commands. They download from the
host of the last `login --host <host>` (or `--host`/`SIMFORGE_HOST`). A map is
5–19 GB and the download prints no progress; pulling an installed version
again downloads it again, so check the cache first.

- A map download is gated by your login: the host checks that your account
  accepted the current agreement, and the first download of a public map
  entitles the workspace. `not_logged_in` means sign in; `entitlement_required`
  (a map that is not public) or `entitlement_revoked` means the account cannot
  download that map, so do not retry or look for another source.
- `--accept-license` records that the user accepted the map's licence; ask
  the user before passing it on their behalf.
- Cache: `--cache-root`, else `$SIMFORGE_MAPS_CACHE_ROOT`, else
  `$XDG_DATA_HOME/simforge/maps`. A release includes its derivatives (the
  schematic view and the ground mesh).
- `simforge maps list` lists the map versions available to you; its JSON
  names each map's id and version.

## Actor assets

```sh
simforge assets pull                              # the closure this runtime is pinned to
simforge assets pull --closure <sha256>           # a specific closure (a package's actorClosureDigest)
```

The closure ships `ATTRIBUTION.json`; the command prints where it is.

## Licences

Map packs are licensed under SimForge's account and download terms: free for
non-commercial research, evaluation and the SimForge challenge, with no
redistribution. The runtime is PolyForm Noncommercial 1.0.0. `simforge runtime
license` prints both.

The vehicle and pedestrian models include CARLA content (© CARLA contributors
and CVC/UAB) and SimForge-made models under CC BY 4.0: if you publish renders
that show them, credit them as `ATTRIBUTION.json` lists. ffmpeg is a separate
program, never bundled.

## Custom vehicle models

`simforge vehicle validate MODEL.GLB` checks a `.glb` against the vehicle model
contract before use (Y up, +X forward, metres, tyres on `y = 0`, origin at the
footprint centre, all four `wheel_*` nodes or none, at most 400,000 triangles,
embedded PNG/JPEG textures up to 2048 px). Each broken rule is a named finding
(`vehicle_misoriented`, `vehicle_not_grounded`, ...) and exits 2. It works
offline. Signed in, `simforge vehicle import` uploads a valid model as the
workspace's own `workspace.<id>`.

## Actor orientation

Every catalog actor faces its heading: heading 0 is +X, Y is up, +Z is right.
The renderer turns each model by its catalog `yawOffsetRad` once, from a front
SimForge reviewed on rendered views of that exact model. Give actors their real
heading and add no per-model rotation; remove any 180 degree compensation an
older closure needed. Heading, route, velocity and sensor mounts are never
changed by a model's orientation. A custom model must say which end is its
front, because an outline is the same both ways round: all four `wheel_*` nodes
with the front pair at +X, or an empty node named `front` at the nose.
`simforge vehicle validate` refuses one facing -X (`vehicle_reversed`) or
declaring nothing (`vehicle_front_undeclared`). The full contract is
`docs/asset-orientation.md` in the SDK.

## Reference

<!-- simforge:reference maps pull, assets pull, vehicle validate -->
_Generated from `simforge --help` (CLI 0.2.0-rc.4) by scripts/skills/skills.py; do not edit._

#### `simforge maps pull`

Download a map your signed-in account is entitled to into the local cache, sha256-verified

Usage: `simforge maps pull <map> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<map>` | required | SimForge-owned map name, optionally pinned as NAME@VERSION |
| `--cache-root CACHE_ROOT` |  | Map cache directory. Default: the SimForge maps cache |
| `--accept-license` |  | Explicitly accept the current license and terms, which include the map pack terms, before downloading |

#### `simforge assets pull`

Pull the actor-asset closure and its ATTRIBUTION.json, verifying every blob's sha256

Usage: `simforge assets pull [flags]`

| Argument / flag | | What |
|---|---|---|
| `--only ONLY` | one of `actors`, `sky` | Pull only this closure. Default: both the actor closure and the sky plates |
| `--closure SHA256` |  | The actor closure digest (sha256). Default: the closure this build is pinned to |
| `--base-url URL` |  | Asset store base URL (https:// or file://). Default: SIMFORGE_ACTOR_ASSETS_BASE_URL, then the public store |
| `--root DIR` |  | Local asset root. Default: SIMFORGE_ACTOR_ASSETS_ROOT, then $XDG_DATA_HOME/simforge/actor-assets |
| `--timeout SECONDS` | default `60` | Seconds to wait for a connection or for a response to start |

#### `simforge vehicle validate`

Check a .glb against the vehicle model contract (frame, size, wheel and body nodes, triangle budget, materials, textures); exit 2 on any error

Usage: `simforge vehicle validate <model.glb> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<model.glb>` | required | The model, a binary glTF (.glb) |
| `--class CLASS` | default `car` | The road-vehicle class it will drive as: car, van, truck or bus (sets the plausible size) |
| `--dims L,W,H` |  | Its real length, width and height in metres (`4.6,1.85,1.45`); the model must match within 10% |

Every command also takes the global flags `--pretty`, `--json`, `--cloud`, `--workspace`, `--idempotency-key`, `--dry-run`, `--api-url`, `--map-version`, `--help`.

Exit codes: `0` ok; `1` command error: the command could not run; `2` findings: the command ran and found something wrong with its input.
<!-- /simforge:reference -->
