---
name: simforge-cloud
description: Run SimForge work on the hosted API with the same `simforge` commands plus the global `--cloud` flag. When to use the cloud, scopes and workspace, API keys, pricing with --dry-run, explicit --max-cost caps and budget refusals, 3D renders, ingesting your own logs (recordings and ROS 2 MCAP), transfer onto other maps, dataset exports, watching runs, downloading results, usage, and the cloud error codes. Use for any hosted SimForge run or when a cloud command fails with an error code.
---

# Hosted runs with --cloud

The commands are the local ones (`simforge-quickstart`, `simforge-render`);
`--cloud` sends them to the hosted API (`/api/v1`) of the host you signed in
to, where SimForge's workers run them. Nothing falls back to the local runtime,
and a local command never silently goes to the cloud.

## When to use --cloud

Train and evaluate locally; use the cloud for what a laptop can't do:

- **Helios Engine 3D renders without a 16 GiB GPU**, or many renders at once:
  `render ... --mode 3d --cloud`.
- **Recast, your own driving logs**: a recording or a ROS 2 MCAP lidar log
  becomes an editable scenario on one of our maps (`import log ... --cloud`).
- **The same event on other maps**: `transfer ... --cloud`. Local transfer needs the map's `odr`
  derivative, which `simforge maps pull` does not install yet (`map_facts_missing`), so use `--cloud`.
- **Training data**: `datasets export` packages a dataset in the public
  dataset format, ready for local training.

RL training stays local (SimForge Gym, the `simforge-rl` package); there is no cloud RL. Every cloud run
is priced before it starts: `--dry-run` prints the current price, and
`--max-cost` caps what a run may cost. Perceiving an MCAP log uses a detector
whose weights are CC BY-NC-SA 4.0, so scenarios made from MCAP logs are for
non-commercial use.

## Sign in with the scopes the task needs

```sh
simforge login --device \
  --scope maps:read --scope scenarios:read --scope scenarios:write \
  --scope runs:read --scope runs:write --scope artifacts:read --scope usage:read
simforge whoami
```

- Runs need `runs:read`, `runs:write` and `artifacts:read`; authoring needs
  `maps:read`, `scenarios:read` and `scenarios:write`.
- A CI job may use an API key instead: `SIMFORGE_API_KEY` (`sfk_…`) and
  `SIMFORGE_API_URL`. It acts in the workspace it was created in.
- Keys are managed with the `keys` commands (need the `keys:admin` scope:
  add `--scope keys:admin` to the login) or in Studio's workspace settings
  (API keys). A new key gets the run scopes unless you pass `--scope`; its
  secret is printed once:

  ```sh
  simforge keys create --name ci --expires-in-days 90
  simforge keys list
  simforge keys revoke <key-id>
  ```
- Your workspace role still applies: a `viewer` only reads; runs need `member`.
- An agent calling `/api/v1` without the CLI signs in for the person through
  the host's `/auth.md` (for example `https://simforge.ai/auth.md`): it
  registers with their email and they approve it with a code. Never ask for
  their password.

## Price, cap, run, download

```sh
simforge simulate s.json --out ./sim --cloud --dry-run
simforge simulate s.json --out ./sim --cloud --max-cost <cents> --wait --download
simforge render s.json --mode 3d --rig rig.json --out ./render --cloud --max-cost <cents> --wait --download
simforge preview clip s.json --mode 3d --rig rig.json --camera <camera> --at 5 --seconds 3 --out cam.mp4 --cloud --max-cost <cents>
simforge render <scenario-id> --revision <revision-id> --mode 2d --out ./render --cloud --max-cost <cents> --wait --download
```

- Price first: `--dry-run` returns `costEstimateCents` and the per-job
  breakdown and starts nothing. On a scenario file it first saves the file to
  the workspace as an immutable revision (free; `created` names it) and records
  that beside the file (`.<file>.cloud-revisions.json`), so the run that
  follows reuses exactly the priced revision. Editing the file makes a new one.
- `--max-cost` is required with `--cloud` and is a deliberate bound no lower
  than the estimate. An estimate above it or the workspace's daily cap is
  refused (`budget_exceeded`), and one its credits do not cover
  (`insufficient_credits`), before anything starts; `--max-cost 0`
  runs only free work. Never raise a cap past the budget you were given.
- A hosted scenario runs at an immutable `--revision` (no "latest"); a revision
  pins the exact map version, actor catalogue and recordings.
- `--wait` follows the run to `succeeded`, `failed`, `partial` or `cancelled`;
  `--download` then writes every artifact into `--out`, sha256-checked.
- Resend a submission whose reply you lost with the same `--idempotency-key`:
  the host answers with the original run and charges once.

## Bring your own logs

```sh
simforge import log drive.recording.json --map auto --name "left turn" --cloud --max-cost <cents> --out ./drive
simforge import log drive.mcap --map auto --cloud --max-cost <cents> --lidar-topic /lidar/top --gnss-topic /gnss/fix
simforge import log drive.mcap --map auto --cloud --dry-run
```

- A recording (`simforge.recording/v1`, JSON or gzipped) is registered onto
  the map when it holds observations, then imported; an MCAP log (ROS 2,
  SimForge MCAP profile) is perceived on a GPU worker first.
- `--map auto` takes the recording's own map, or the map the log's GNSS path
  lies in; otherwise name it (`name@vN`).
- `--lidar-topic`/`--gnss-topic` choose among several topics;
  `--ego-dims L,W,H --ego-rear-axle-to-centre M` give the ego's box.
- The result is the new scenario, its import report and its event count;
  `--out` downloads the scenario document, import report and Event Timeline.
  A dry run uploads the log too, so the price reflects its length.

## Transfer onto another map

```sh
simforge transfer s.json --map yale-street@v9 --cloud --max-cost <cents> --out ./yale
simforge transfer <scenario-id> --revision <revision-id> --map el-camino-road --cloud --max-cost <cents> --wait
```

- A transfer that lands creates a new scenario in the workspace; one no site
  accepts creates nothing and exits 2 with its verdict or refusal code.
- `--sites network|corridor`, `--profile pure|generalized|key-only` and
  `--crossing any|faithful` mean what they mean locally.

## Export a dataset for training

```sh
simforge datasets export <dataset-id> --out ./export
simforge datasets status <export-id>
simforge datasets pull <export-id> --out ./export
```

The export is the public dataset format (`simforge.dataset-export/v1`); load
it locally for training.

## Follow up on runs

```sh
simforge runs watch <run-id>
simforge runs cancel <run-id>
simforge results download <run-id> --out ./results
simforge scenarios list --limit 20
simforge scenarios show <scenario-id>
simforge usage
```

A partial or cancelled run keeps the artifacts it produced.

## Common error codes

| Code | Meaning | Do |
|---|---|---|
| `not_logged_in` | no login for this host | `simforge login --host <host>` |
| `max_cost_required` | `--cloud` without a cost cap | pass `--max-cost <cents>` deliberately |
| `budget_exceeded` | estimate above `--max-cost` (`detail.limit: max_cost`) or the workspace's daily cap (`daily_cap`); the reason says it in dollars | raise the cap only within budget, wait for the daily reset (00:00 UTC), or stop |
| `insufficient_credits` | the workspace's credit does not cover a paid run (3D render, evaluation, transfer, Recast, asset generation); `detail.creditsUrl` is the credits page | stop and tell the user; free work (2D renders, local runs) needs no credits |
| `cloud_not_open` | the host's cloud is off, or no worker there runs that work yet (`detail.code` `run_kind_unavailable`, `detail.kinds`) | run locally without `--cloud` |
| `email_unverified` | the account's email is not verified; runs and downloads wait on it (`detail.resendUrl` resends the link) | stop and tell the user to open the link in their email or resend it from `detail.resendUrl`; do not retry until they say it is verified |
| `workspace_mismatch` | `--workspace` names another workspace (`insufficient_scope` with this `detail.code`) | sign in for that workspace's organization |
| `insufficient_scope` | missing scope (`detail.required`) or role (`detail.requiredRole`) | sign in again with the scope, or ask an admin |
| `rate_limited` | a free 2D render over the account's limit: one at a time (`detail.limit: free_2d_concurrent`) or 30 a day (`free_2d_daily`) | wait for the running one, or the daily reset (00:00 UTC) |
| `not_found` | the object does not exist or is not visible | check the id and workspace |
| `conflict` | stale version or digest, or a duplicate | re-read, then decide; never retry blindly |

## Reference

<!-- simforge:reference simulate, import log, keys create, runs watch, results download, usage -->
_Generated from `simforge --help` (CLI 0.2.0-rc.4) by scripts/skills/skills.py; do not edit._

#### `simforge simulate`

Simulate the same scenario file locally or with --cloud

Usage: `simforge simulate <target> <extra> [flags]`

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
| `--seed SEED` |  | Override the resolution's seed (the trace is then expected to differ) |
| `--map-dir DIR` |  | Use this map directory instead of the installed map found by content (map.xodr[.gz] sha256 = the manifest's map.xodrSha256) |
| `--cache-root DIR` |  | Map cache root to search. Default: SIMFORGE_MAPS_CACHE_ROOT, then $XDG_DATA_HOME/simforge/maps |
| `--trace FILE` |  | Instance mode (the positional is an instance file): also write the trace here (gzip JSON) |
| `--allow-no-colliders` |  | Simulate without map colliders when the map publishes none (recorded in the result; the default refuses) |

#### `simforge import log`

A recording (simforge.recording/v1 or log-scene/v1) as a scenario package: scenario, recording, trace, import report

Usage: `simforge import log <recording> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<recording>` | required | The recording (JSON, optionally gzipped). With --cloud: The log: a recording (JSON, optionally gzipped) or a ROS 2 MCAP lidar log |
| `--map DIR|NAME@VERSION` | required | The installed map: a map directory, or name[@version] under the maps cache (semantic profile) |
| `--out DIR` | required | The package directory to write (created; must be empty if it exists) |
| `--report REPORT.JSON` |  | The register step's import report (`simforge.import-report/v1` fields: methods, registration, laneMatch, droppedActors), merged into the package's |
| `--cache-root DIR` |  | Map cache root to search for NAME@VERSION. Default: SIMFORGE_MAPS_CACHE_ROOT, then $XDG_DATA_HOME/simforge/maps |
| `--assets-root DIR` |  | Actor-asset root whose closure restricts catalog binding to renderable models. Default: SIMFORGE_ACTOR_ASSETS_ROOT, then $XDG_DATA_HOME/simforge/actor-assets |
| `--actor-closure SHA256` |  | The actor closure the package pins (default: this CLI's pinned public closure) |
| `--allow-no-colliders` |  | Simulate without map colliders when the map publishes none (recorded; the default refuses) |
| `--name NAME` |  | The scenario's name. Default: the file's name |
| `--max-cost CENTS` |  | Explicit maximum cloud cost in cents |
| `--lidar-topic TOPIC` |  | MCAP only: the lidar topic, when the log has several |
| `--gnss-topic TOPIC` |  | MCAP only: the GNSS topic, when the log has several |
| `--ego-dims L,W,H` | repeatable | MCAP only: the ego's box in metres, length,width,height (perceive's default otherwise) |
| `--ego-rear-axle-to-centre METRES` |  | MCAP only: how far the ego box's centre is ahead of base_link (the rear axle), metres |

#### `simforge keys create`

Create an API key for this workspace; its secret is printed once

Usage: `simforge keys create [flags]`

| Argument / flag | | What |
|---|---|---|
| `--name NAME` | required | A name to recognize the key by (1-200 characters) |
| `--scope SCOPE` | repeatable | A scope the key holds (repeatable). Default: maps:read scenarios:read scenarios:write datasets:read runs:read runs:write artifacts:read usage:read |
| `--expires-in-days DAYS` |  | Expire the key after this many days (1-3650). Default: no expiry |

#### `simforge runs watch`

Follow a hosted run until it succeeds, fails, is partial or is cancelled

Usage: `simforge runs watch <id> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<id>` | required | The id |

#### `simforge results download`

Download a hosted run's artifacts, sha256-checked

Usage: `simforge results download <run_id> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<run_id>` | required | The hosted run |
| `--out OUT` | default `.` | Directory to write the artifacts into |

#### `simforge usage`

Hosted usage and spend of the signed-in workspace

Usage: `simforge usage [flags]`

| Argument / flag | | What |
|---|---|---|
| `--from FROM` |  | Start of the period (ISO 8601 date or time) |
| `--to TO` |  | End of the period (ISO 8601 date or time) |

Every command also takes the global flags `--pretty`, `--json`, `--cloud`, `--workspace`, `--idempotency-key`, `--dry-run`, `--api-url`, `--map-version`, `--help`.

Exit codes: `0` ok; `1` command error: the command could not run; `2` findings: the command ran and found something wrong with its input.
<!-- /simforge:reference -->
