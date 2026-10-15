---
name: simforge-closed-loop
description: Run local closed-loop SimForge episodes through the separate runtime and SimForge Gym, the open simforge-rl Gymnasium client. Use seeded scenario sets, independent vector environments, explicit state or sensor observations, reward hooks and PPO rollout/checkpoint/replay. Cloud interactive RL is not part of this version; env serve --cloud is refused rather than replaced by a scripted policy job.
---

# Local reinforcement learning

SimForge Gym, the open `simforge-rl` package, does not link an engine or renderer. `simforge env
serve` launches the installed closed runtime and exposes the existing binary
`simforge.env-serve/v1` socket. Runtime protocol v1 also supports environment
handles over stdio. Neither path silently switches to cloud.

## Start a session

Install the runtime and the scenario's map through the signed-in distribution
flow in `simforge-quickstart`. Then:

```sh
python -m pip install 'simforge-rl[ppo]'
simforge env serve scenario.json --socket /tmp/simforge-env.sock --no-sensors --max-sessions 3 --json
```

`simforge-rl` reaches PyPI with the open release (PyPI holds only a
placeholder until then); before that, install it from a toolkit checkout
(`python -m pip install './clients/rl[ppo]'`). CPU torch is enough for smoke
runs.

The server prints a ready receipt and runs until stopped. Connect from another
terminal. Use an absolute socket path and stop only the server you started.

```python
from simforge_rl import SimForgeEnv, SocketTransport, WeightedTerms

with SimForgeEnv(
    SocketTransport('/tmp/simforge-env.sock'),
    action_mode='setpoint',
    observation='state',
    reward_fn=WeightedTerms({'progress': 1.0, 'proximity': 2.0}),
) as env:
    observation, info = env.reset(seed=7)
    observation, reward, terminated, truncated, info = env.step([5.0, 1.0])
```

A setpoint action is `[speed_mps, accel_mps2]`; `None` delegates to authored
control. Reset before stepping and do not invent state-vector offsets: use the
runtime's declared observation space. An explicit reset is required after a
terminated or truncated episode. `info["end"]` names why it ended (`collision`,
`goal`, `off_map`, `clip_end`, `decision_limit`): a policy that drives the ego
off the map ends the episode `truncated` with `off_map`, not an error.

## Sensors and state

For rendered observations, serve with `--rig rig.json` instead of `--no-sensors`
and use `observation='sensors'` or `'state+sensors'`. Sensor keys are
`<sourceId>/<pass>`. Privileged state is supervision, not a rendered policy
input: do not accidentally expose it to a sensor-only policy. The chosen render
preset and explicit hardware/software-adapter policy remain in effect.

## Vector tasks and PPO

`ScenarioSet` stores weighted, seeded task distributions; `ScenarioServers`
starts independent local sessions, and `make_vector_env` builds synchronous or
asynchronous Gymnasium vectors. Use held-out seeds for evaluation and keep the
scenario/map/runtime pins with the checkpoint. The server's `--max-sessions`
must accommodate vector environments plus the evaluation environment.

```sh
python -m simforge_rl.examples.ppo --endpoint /tmp/simforge-env.sock --num-envs 2 --total-timesteps 256 --checkpoint ppo.pt --json
```

The example performs a rollout, PPO update, checkpoint and held-out replay; use
`--scenario-set tasks.json` for a pinned distribution. GPU training is optional;
small qualification runs use CPU. Do not claim throughput from a smoke run.

## Agent interfaces

Finite CLI operations are also exposed by `simforge mcp`. A long-running
`env serve` is started in a terminal, not inside a blocking MCP tool call. The
open Python runtime client supports `RuntimeClient.spawn()` and
`RuntimeEnvTransport(runtime, [workspace, '--no-sensors'])` when no binary socket
is needed. Cloud `closed-loop` is a bounded queued policy evaluation, not an
interactive environment session.

## Reference

<!-- simforge:reference env serve -->
_Generated from `simforge --help` (CLI 0.2.0-rc.4) by scripts/skills/skills.py; do not edit._

#### `simforge env serve`

Serve reset/step/observe episodes with rendered sensors over a Unix socket

Usage: `simforge env serve <workspace> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<workspace>` | required | The scenario package or workspace directory (a package without a simulation is compiled from its document and recordings) |
| `--socket PATH` | required | The Unix socket to listen on (refused if another server is live on it) |
| `--rig RIG.JSON` |  | The sensor rig (`simforge.render-rig/v1`, as `simforge render` takes it) |
| `--no-sensors` |  | Serve the episode without a renderer (observations are the engine's only) |
| `--preset PRESET` | one of `training`, `showcase`; default `training` | The render preset for observations (`schematic` is offline `simforge render` only) |
| `--passes PASSES` | one of `rgb`, `id`, `depth`, `semantic`; default `rgb`; repeatable | Camera passes rendered per observation, comma-separated |
| `--textures TEXTURES` | one of `uastc-full`, `bc7-512`; default `uastc-full` | The staged texture tier |
| `--episode EPISODE.JSON` |  | Episode configuration (camelCase `EpisodeConfig`, as the gym's episode specs carry it) |
| `--decision-hz HZ` |  | Policy decisions per second (overrides the episode's `decisionHz`) |
| `--max-objects N` | default `64` | Rows of the `objects` observation |
| `--allow-software-adapter` |  | Render on a software (CPU) Vulkan adapter such as lavapipe, explicitly |
| `--map-dir DIR` |  | Use this map directory instead of the installed map found by content |
| `--cache-root DIR` |  | Map cache root to search. Default: SIMFORGE_MAPS_CACHE_ROOT, then $XDG_DATA_HOME/simforge/maps |
| `--assets-root DIR` |  | Actor-asset root. Default: SIMFORGE_ACTOR_ASSETS_ROOT, then $XDG_DATA_HOME/simforge/actor-assets |
| `--actor-closure SHA256` |  | The actor closure to bind, instead of the workspace's `catalog.actorClosureDigest` |
| `--actor-layer SHA256` |  | The workspace's private model layer (an actor closure in the same store) for `workspace.*` models, instead of the workspace's `simforge.actorLayer` extension |
| `--vram-budget BYTES` |  | A texture budget in bytes; without it admission is skipped (reported) |
| `--log FILE` |  | Where the renderer's diagnostics go. Default: `<socket>.log` |
| `--allow-map-drift` |  | Render on the installed map release even where its members differ from the workspace's map closure (recorded; the default refuses) |
| `--max-sessions N` | default `8` | Maximum concurrent client sessions sharing one prewarmed renderer. Training defaults to eight sessions per map renderer |
| `--shm-size-mb MIB` | default `512` | Size of the renderer's shared-memory frame ring, MiB |

Every command also takes the global flags `--pretty`, `--json`, `--cloud`, `--workspace`, `--idempotency-key`, `--dry-run`, `--api-url`, `--map-version`, `--help`.

Exit codes: `0` ok; `1` command error: the command could not run; `2` findings: the command ran and found something wrong with its input.
<!-- /simforge:reference -->
