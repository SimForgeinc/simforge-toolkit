---
name: simforge-authoring
description: Author a SimForge driving scenario JSON without writing JSON by hand. Create it on an installed map with `simforge scenario new`, apply typed authoring operations (add/move/duplicate/remove actors, ego, speed, static, lane change, next-junction turn, crossing/waypoint route, event timing, weather, clock, clip length) atomically with version and digest checks, validate and preview it; or do the same on a hosted workspace draft and freeze an immutable revision. Use whenever asked to build, edit or check a scenario, locally or through the cloud CLI or MCP.
---

# Authoring scenarios

Every change is a typed **operation** from one shared catalogue: the local
`scenario` commands and the hosted draft API run the same operations, with the
same placement, ego, motion and timing rules as Studio's editor. Every refusal
is a stable `code` you can repair on. Never edit the scenario JSON by hand.

## Local: a scenario file on an installed map

Needs the local runtime and the map (`simforge runtime install`,
`simforge maps pull <map>@<version>`).

```sh
simforge scenario new --map <map> --out s.json --clip-seconds 20
simforge scenario ops list                 # every operation; `ops show <op>` for flags, bounds, refusal codes
simforge scenario actors                   # built-in actors: class, dims, ego/static eligibility
simforge scenario apply s.json add-actor --id ego --catalog-id vehicle.sedan --lane-rsl <road>:<section>:<lane> --s 20 \
  -- set-ego --role-id ego \
  -- add-actor --id cutter --catalog-id vehicle.suv --lane-rsl <left-lane-rsl> --s 30 \
  -- add-lane-change --id lc --actor-id cutter --direction right --start-s 4 \
  -- set-weather --weather light_rain -- set-clock --minutes 1035
simforge scenario apply s.json add-actor --id ped --catalog-id pedestrian.adult --x <x> --z <z> \
  -- set-waypoint-route --id cross --actor-id ped --point <x>,<z> --point <x2>,<z2> --start-s 2 --duration-s 6
simforge scenario apply s.json --expect-version <version> --expect-digest <digest> \
  add-next-junction-turn --id turn --actor-id ego --direction left --start-s 0   # checks go before the first operation
simforge scenario check s.json --strictness warnings --preview   # validation + engine run of every actor
simforge scenario drive s.json                                    # how each road vehicle drove
simforge scenario pull <scenario-id> --out s.json                 # a hosted scenario as a local file (scenarios:read)
```

Then preview and render the file locally or with `--cloud`
(`simforge-render`).

## Hosted: a workspace draft and an immutable revision

Sign in with `maps:read`, `scenarios:read` and `scenarios:write`
(`simforge-cloud`). These commands are always hosted.

```sh
simforge authoring capabilities                       # your scopes, role, available operations
simforge authoring operations add-actor               # flags, bounds, schemas of one operation
simforge maps list
simforge authoring discover --map <map-version-id> --kind lanes --limit 50   # or locations, movements, catalogue
simforge authoring spawn --map <map-version-id> --catalog-id <catalog-id> --lane-rsl <lane-rsl> --s 30
simforge drafts create --map <map-version-id> --name "cut-in at junction 4" --clip-seconds 20
simforge drafts apply <draft-id> --expected-version <draftVersion> --expected-digest <contentDigest> \
  add-actor --id ego --catalog-id <car-catalog-id> --lane-rsl <lane-rsl> --s 20 --speed-kph 40 \
  -- set-ego --role-id ego -- set-clip --clip-seconds 20
simforge drafts show <draft-id>
simforge drafts validate <draft-id> --strict --preview
simforge drafts save <draft-id> --expected-version <n> --expected-digest <digest>
simforge drafts freeze <draft-id> --expected-version <n> --expected-digest <digest> --name "v1"
```

- `drafts create` prints the draft's `draftVersion` (1 for a new draft) and
  `contentDigest`: pass exactly those to the first `apply`, then the values
  each result prints. Another digest (an operation digest) is a
  `draft_conflict`.
- `authoring discover --limit` is at most 100.
- `drafts save` returns the immutable `revisionId` (with its map, actor and
  recording pins); run it with `--revision <revision-id>` and `--cloud`
  (`simforge-cloud`). The scenario id is the draft's `documentId`.
- An operation the host reports unavailable is refused with
  `operation_unavailable`; do not emulate it.

## Operation rules (local and hosted)

- `apply` is one atomic transaction: chain operations with `--`; all apply or
  none. The result is the new version and digest, the changed paths and ids,
  and the diagnostics.
- Every write is a compare-and-swap. Locally pass the `version`/`digest` a
  previous result printed as `--expect-version`/`--expect-digest`, right after
  the file and before the first operation (after it they are
  `unknown_flag`); hosted
  drafts require `--expected-version`/`--expected-digest`. A stale view is a
  conflict (exit 2): re-read, decide, never re-apply in a loop.
- Moving road vehicles need a lane (`--lane-rsl` + `--s`); pedestrians and
  static actors take a scene point (`--x` + `--z`, `x` east, `z` = minus north).
  Road-vehicle overlap is refused (`spawn_overlap`).
- `add-next-junction-turn` is a turn event at the next junction; placing an
  actor on a turn lane is not a turn.
- `set-waypoint-route` with two points is a crossing whose first point is the
  actor's position; an impossible duration is `route_timing_infeasible`.
- Event times are seconds from clip start inside the clip
  (`event_time_outside_clip`); `set-clip` refuses to cut an event.
- Enum values and bounds come from `scenario ops show <op>` or
  `authoring operations <op>`; do not guess them.

Repair loop: read each diagnostic's `code`, `path` and `actorId`, change the
operation that caused it, and apply again against the current version and
digest. Readiness is a clean `check --preview` (local) or
`drafts validate --strict --preview` (hosted).

## MCP

`simforge mcp` exposes these same commands as tools (`scenario_apply`,
`scenario_check`, `drafts_apply_add-actor`, ...); chain several operations in
one transaction with the `simforge` tool's raw `args`.

## Reference

<!-- simforge:reference scenario new, scenario apply, scenario check, authoring operations, drafts create, drafts apply add-actor, drafts validate, drafts save -->
_Generated from `simforge --help` (CLI 0.2.0-rc.4) by scripts/skills/skills.py; do not edit._

#### `simforge scenario new`

Create an empty scenario on an installed map (revision 0)

Usage: `simforge scenario new [flags]`

| Argument / flag | | What |
|---|---|---|
| `--map MAP` | required | The installed map (`simforge maps pull <map>@<version>`) |
| `--out FILE` | required | Where to write the scenario document |
| `--name NAME` | default `Untitled scenario` | Scenario name |
| `--clip-seconds S` | default `20` | Clip length, seconds (3 to 120) |
| `--created-at TIME` |  | Creation timestamp `YYYY-MM-DDTHH:MM:SS.mmmZ` (default: now), for reproducible documents |
| `--force` |  | Overwrite an existing file |

#### `simforge scenario apply add-actor`

Place a new actor: a road vehicle on a semantic lane, or a pedestrian/free static actor at a scene point.

Usage: `simforge scenario apply add-actor <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--body-color BODY_COLOR` |  | Presentation body colour `#rrggbb`. |
| `--catalog-id CATALOG_ID` | required | Built-in actor (`simforge scenario actors`): vehicle.sedan, pedestrian.adult, ... |
| `--heading-rad N` |  | Heading in the scene frame; default: the lane heading or the actor class default. [rad] |
| `--id ID` | required | New actor id (never allocated implicitly). |
| `--label LABEL` |  | Display label. |
| `--lateral-m N` |  | Lateral offset in the lane, positive to the left of travel. [m] |
| `--opposing` |  | Snap to the nearest genuinely opposing lane. |
| `--overlap-override` |  | Accept overlap with another actor (free/static actors only; refused for moving road vehicles). |
| `--speed-kph N` |  | Initial speed; default: posted limit for road vehicles. [0..160 km/h] |
| `--static` |  | Place as a static actor (no motion, zero speed). |
| `--y N` |  | Explicit height; otherwise the map ground at (x,z) is used and refused without ground. [m] |
| `--lane-rsl LANE_RSL` |  | Semantic lane `road:section:lane` (with s). |
| `--s N` |  | Station along laneRsl in storage direction. [>= 0 m] |
| `--x N` |  | Scene-frame x (east). [m] |
| `--z N` |  | Scene-frame z (minus north). [m] |

#### `simforge scenario apply add-lane-change`

Add a timed lane change for a mobile lane-bound road actor.

Usage: `simforge scenario apply add-lane-change <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--actor-id ACTOR_ID` | required | Actor. |
| `--direction DIRECTION` | required; one of `left`, `right` | Target lane side. |
| `--duration-s N` |  | Lane-change duration. [> 0 s] (default 3) |
| `--end-s N` |  | Clip time the event ends. [>= 0 s] |
| `--id ID` | required | New event id. |
| `--start-s N` | required | Clip time the change starts. [>= 0 s] |

#### `simforge scenario apply add-next-junction-turn`

Choose the turn an actor takes at its next junction.

Usage: `simforge scenario apply add-next-junction-turn <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--actor-id ACTOR_ID` | required | Actor. |
| `--direction DIRECTION` | required; one of `left`, `right`, `straight` | Movement at the next junction. |
| `--end-s N` |  | Clip time the event ends. [>= 0 s] |
| `--id ID` | required | New event id. |
| `--start-s N` | required | Clip time the choice applies from. [>= 0 s] |

#### `simforge scenario apply duplicate-actor`

Copy an actor with its presentation and own events under a new id at a new position.

Usage: `simforge scenario apply duplicate-actor <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--heading-rad N` |  | Heading in the scene frame; default: the lane heading or the actor class default. [rad] |
| `--id ID` | required | Source actor. |
| `--lateral-m N` |  | Lateral offset in the lane, positive to the left of travel. [m] |
| `--new-id NEW_ID` | required | New actor id. |
| `--overlap-override` |  | Accept overlap with another actor (free/static actors only; refused for moving road vehicles). |
| `--y N` |  | Explicit height; otherwise the map ground at (x,z) is used and refused without ground. [m] |
| `--lane-rsl LANE_RSL` |  | Semantic lane `road:section:lane` (with s). |
| `--s N` |  | Station along laneRsl in storage direction. [>= 0 m] |
| `--x N` |  | Scene-frame x (east). [m] |
| `--z N` |  | Scene-frame z (minus north). [m] |

#### `simforge scenario apply move-actor`

Move an actor to a lane station or scene point, keeping identity, class, static state and speed.

Usage: `simforge scenario apply move-actor <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--heading-rad N` |  | Heading in the scene frame; default: the lane heading or the actor class default. [rad] |
| `--id ID` | required | Actor to move. |
| `--lateral-m N` |  | Lateral offset in the lane, positive to the left of travel. [m] |
| `--overlap-override` |  | Accept overlap with another actor (free/static actors only; refused for moving road vehicles). |
| `--y N` |  | Explicit height; otherwise the map ground at (x,z) is used and refused without ground. [m] |
| `--lane-rsl LANE_RSL` |  | Semantic lane `road:section:lane` (with s). |
| `--s N` |  | Station along laneRsl in storage direction. [>= 0 m] |
| `--x N` |  | Scene-frame x (east). [m] |
| `--z N` |  | Scene-frame z (minus north). [m] |

#### `simforge scenario apply remove-role`

Delete an actor with dependent events, reasoning and ego handoff.

Usage: `simforge scenario apply remove-role <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--id ID` | required | Actor to delete. |

#### `simforge scenario apply set-actor-speed`

Set an actor's initial speed.

Usage: `simforge scenario apply set-actor-speed <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--id ID` | required | Actor. |
| `--speed-kph N` | required | Initial speed. [0..160 km/h] |

#### `simforge scenario apply set-actor-static`

Make an actor static (clears its route and motion events) or mobile again.

Usage: `simforge scenario apply set-actor-static <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--id ID` | required | Actor. |
| `--static BOOL` | required; one of `true`, `false` | true: static; false: mobile. |

#### `simforge scenario apply set-clip`

Set the clip length and/or warm-up; out-of-clip events are refused, never truncated.

Usage: `simforge scenario apply set-clip <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--clip-seconds N` |  | Clip length. [3..120 s] |
| `--warmup-seconds N` |  | Warm-up before the clip. [0..30 s] |

#### `simforge scenario apply set-clock`

Set the precise scene clock (and the matching time-of-day preset).

Usage: `simforge scenario apply set-clock <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--minutes N` | required | Minutes after local midnight. [0..1439 min] |

#### `simforge scenario apply set-ego`

Make an actor the ego vehicle.

Usage: `simforge scenario apply set-ego <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--role-id ROLE_ID` | required | Actor to make ego. |

#### `simforge scenario apply set-event-timing`

Retime an event's start and optional end.

Usage: `simforge scenario apply set-event-timing <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--end-s N` |  | End clip time. [>= 0 s] |
| `--id ID` | required | Event. |
| `--start-s N` | required | Start clip time. [>= 0 s] |

#### `simforge scenario apply set-waypoint-route`

Give an actor a smooth stop-to-stop timed route through scene points (two points: a crossing).

Usage: `simforge scenario apply set-waypoint-route <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--actor-id ACTOR_ID` | required | Actor. |
| `--duration-s N` | required | Route duration. [> 0 s] |
| `--end-s N` |  | Clip time the event ends. [>= 0 s] |
| `--id ID` | required | New event id. |
| `--point X,Z` | required; repeatable | Scene points, the actor's start first. Repeat once per item, at least 2. |
| `--start-s N` | required | Clip time the route starts. [>= 0 s] |

#### `simforge scenario apply set-weather`

Set the scenario weather.

Usage: `simforge scenario apply set-weather <file> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document (edited in place, atomically) |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expect-version N` |  | Refuse unless the document is at this revision (`stale_version`) |
| `--expect-digest SHA256` |  | Refuse unless the document content has this digest (`stale_digest`); also accepts an edit made outside simforge |
| `--transaction-label TEXT` |  | The transaction's undo label (default: the operations' summaries) |
| `--weather WEATHER` | required; one of `clear`, `cloudy`, `overcast`, `light_rain`, `heavy_rain`, `wet_road`, `fog_light`, `fog_dense`, `snow`, `sleet` | Canonical scenario weather. |

#### `simforge scenario check`

Full validation on the document's map (exit 2 per --strictness); --preview also runs every actor on the engine

Usage: `simforge scenario check <file> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<file>` | required | The scenario document |
| `--strictness STRICTNESS` | required; one of `errors`, `warnings` | What fails the check (exit 2) |
| `--preview` |  | Also materialize and run every actor on the engine (collisions, quality, input identity) |

#### `simforge authoring operations`

Every operation with its `drafts apply` flags, result schema and the host's status for this credential

Usage: `simforge authoring operations <op_id> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<op_id>` | optional | One operation id (camelCase or its kebab-case command); absent, every operation |

#### `simforge drafts create`

Create a workspace draft on an exact map version; prints its receipt (documentId, draftVersion, contentDigest, mapPin)

Usage: `simforge drafts create [flags]`

| Argument / flag | | What |
|---|---|---|
| `--map MAP_VERSION_ID` | required | The map version id the draft is pinned to |
| `--name NAME` | required |  |
| `--dataset DATASET_ID` |  | The writable dataset; absent, the credential's default dataset (named in the receipt) |
| `--clip-seconds S` |  |  |

#### `simforge drafts apply add-actor`

Place a new actor: a road vehicle on a semantic lane, or a pedestrian/free static actor at a scene point.

Usage: `simforge drafts apply add-actor <draft_id> <operation> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<draft_id>` | required |  |
| `<operation>` | optional | Further operations of the same transaction, each after `--`. |
| `--expected-version N` | required | The draftVersion of the receipt this change is based on |
| `--expected-digest SHA256` | required | The contentDigest of the receipt this change is based on |
| `--body-color BODY_COLOR` |  | Presentation body colour `#rrggbb`. |
| `--catalog-id CATALOG_ID` | required | Built-in actor (`simforge scenario actors`): vehicle.sedan, pedestrian.adult, ... |
| `--heading-rad N` |  | Heading in the scene frame; default: the lane heading or the actor class default. [rad] |
| `--id ID` | required | New actor id (never allocated implicitly). |
| `--label LABEL` |  | Display label. |
| `--lateral-m N` |  | Lateral offset in the lane, positive to the left of travel. [m] |
| `--opposing` |  | Snap to the nearest genuinely opposing lane. |
| `--overlap-override` |  | Accept overlap with another actor (free/static actors only; refused for moving road vehicles). |
| `--speed-kph N` |  | Initial speed; default: posted limit for road vehicles. [0..160 km/h] |
| `--static` |  | Place as a static actor (no motion, zero speed). |
| `--y N` |  | Explicit height; otherwise the map ground at (x,z) is used and refused without ground. [m] |
| `--lane-rsl LANE_RSL` |  | Semantic lane `road:section:lane` (with s). |
| `--s N` |  | Station along laneRsl in storage direction. [>= 0 m] |
| `--x N` |  | Scene-frame x (east). [m] |
| `--z N` |  | Scene-frame z (minus north). [m] |

#### `simforge drafts validate`

Validate every actor of the draft, optionally with the authoritative preview; exit 2 on findings

Usage: `simforge drafts validate <draft_id> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<draft_id>` | required |  |
| `--strict` |  | Warnings are findings too |
| `--preview` |  | Run the authoritative materializer/engine preview (every actor, collisions, quality issues) |

#### `simforge drafts save`

Save the draft through the workspace store at an explicit version and digest

Usage: `simforge drafts save <draft_id> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<draft_id>` | required |  |
| `--expected-version N` | required | The draftVersion of the receipt this change is based on |
| `--expected-digest SHA256` | required | The contentDigest of the receipt this change is based on |

Every command also takes the global flags `--pretty`, `--json`, `--cloud`, `--workspace`, `--idempotency-key`, `--dry-run`, `--api-url`, `--map-version`, `--help`.

Exit codes: `0` ok; `1` command error: the command could not run; `2` findings: the command ran and found something wrong with its input.
<!-- /simforge:reference -->
