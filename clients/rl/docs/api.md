# simforge-rl API reference

Everything below is importable from `simforge_rl`. Episodes run in the closed SimForge runtime on your machine; this package is the open client and never substitutes a simulator: no runtime, no episode. A refusal is an `EnvServeError` with a stable `code`.

## Environments

### `SimForgeEnv(transport, *, action_mode="setpoint", observation="state", reward_fn=native, info_channel=True, seed=None)`

One episode session as a Gymnasium `Env`. `terminated` and `truncated` are the runtime's.

| Argument | Values |
|---|---|
| `transport` | `SocketTransport(path)` (a `simforge env serve` socket) or `RuntimeEnvTransport(runtime, argv)` |
| `action_mode` | `"setpoint"`: `[target_speed_mps, target_accel_mps2]`; `"control"`: `[throttle, brake, steer]`. `step(None)` keeps the scenario's authored ego control. |
| `observation` | `"state"`: `state_vector` (ego x, y, cos/sin heading, speed, acceleration, lane offset, lateral rate, route arc length, nearest range), `objects` (`[range_m, bearing_rad, range_rate_mps, los, valid]` rows), `bev` when enabled. `"sensors"`: rendered passes keyed `"<sourceId>/<pass>"` (`rgb`/`id`/`semantic` uint8 HxWx4, `depth` float32 HxW). `"state+sensors"`: both. |
| `reward_fn` | `fn(native_reward, info) -> float`; see [Rewards](#rewards) |

Methods: `reset(seed=, options=)` (`options["episode_seed"]` fixes the runtime seed), `step(action)`, `observe()`, `checkpoint() -> bytes`, `restore(bytes)`, `close()`. In `"sensors"` mode `info` carries only `t_s`, `sensors`, `render_ms` and `native_reward`: no engine state, object ids, reward terms or events.

`action_space_for(mode)`, `state_space(hello)` and `sensor_space(hello)` build the spaces from a runtime hello.

### `ScenarioSetEnv(scenario_set, transports=None, *, max_open=None, **env_kwargs)`

A `SimForgeEnv` per scenario; each `reset` draws a scenario by weight and an episode seed from the env's seeded generator, so `reset(seed=s)` fixes the whole sequence of draws. `info["scenario"]` and `info["episode_seed"]` name the draw; `reset(options={"scenario_name": ..., "episode_seed": ...})` pins it. Every scenario must have the same spaces.

`transports` maps scenario names to a socket path or a zero-argument factory returning a transport. With `max_open=None` every session opens at construction. With `max_open=k` a session opens when its scenario is first drawn and at most `k` stay open, least recently used closing first: the way to train on thousands of exported scenarios.

### `make_vector_env(scenario_set, transports, num_envs, *, vectorization="async", wrappers=None, **env_kwargs)`

`num_envs` `ScenarioSetEnv`s as a Gymnasium `AsyncVectorEnv` (spawned workers) or `SyncVectorEnv`, with same-step autoreset: an ended sub-env returns its next episode's first observation and keeps the final one in `info["final_obs"]`. `transports` is one mapping for all sub-envs or a list of one per sub-env (`ScenarioServers.socket_sets`, `RuntimeSessions.transports`). `env_kwargs` go to each `ScenarioSetEnv` (`max_open`, `observation`, `reward_fn`, ...).

### `SimForgeBatchVectorEnv(runtime, argv, num_envs, *, shards=None, cwd=None, action_mode="setpoint", reward_fn=native, info_channel=True)`

Up to 64 state-only episodes of one workspace inside one runtime, sharded over CPU threads, as a `VectorEnv` with next-step autoreset. A row the runtime refuses raises `EnvServeError` (`code` `batch_rows_failed`, `detail` `{env index: native code}`).

## Task sets

### `Scenario(name, *, workspace=None, template=None, endpoint=None, weight=1.0, seeds=(), site=None)`

One task: exactly one of a scenario workspace directory, a scenario template file (compiled on a map when served) or a running `env serve` endpoint. `seeds` fixes the episode seeds the task draws from (else uniform). `site` pins a template to one site of its map version (an id from `simforge sites match`); without it the template compiles at the map's best site.

### `ScenarioSet`

| | |
|---|---|
| `ScenarioSet.of(scenarios, map=None)` | a weighted task distribution; weights must be finite, non-negative and not all zero |
| `ScenarioSet.bundled(name)` | the task set shipped for a released map; `bundled_sets()` lists the names |
| `ScenarioSet.from_export(source, *, split="train", map_name=None, cache_dir=None, binary=None)` | every scenario of a dataset export (see [Dataset exports](#dataset-exports)) |
| `ScenarioSet.load(path)`, `.save(path)` | `simforge-rl.scenario-set/v1` files; relative paths resolve against the file; unknown fields are refused |
| `.sample(rng)`, `.index(name)`, `.scenarios`, `.map` | |

`MapPin(name, version, xodr_sha256)` is the map a set's templates were verified on. Without an explicit `map_dir`, a pinned set's templates compile on the installed map with that OpenDRIVE digest (installed with `simforge maps pull NAME@VERSION`); a different version fails with `map_not_found` rather than compiling elsewhere.

### `ScenarioServers(scenario_set, *args, sessions=8, replicas=1, map_dir=None, socket_dir=None, binary=None, env=None)`

Starts `replicas` `simforge env serve` processes per workspace or template scenario (`args` are passed to each, e.g. `"--no-sensors"`); endpoint scenarios pass through. `.sockets` is one scenario-to-socket mapping, `.socket_sets(n)` spreads `n` consumers round-robin over replicas. A server handles its sessions on one core and needs `sessions` at least the number of sub-envs it serves. Use as a context manager.

### `RuntimeSessions(scenario_set, *args, binary=None, map_dir=None, workdir)`

Picklable per-scenario transport factories over one `simforge-runtime serve --stdio` per process (each vector worker starts its own). `.transports()` feeds `make_vector_env` and `ScenarioSetEnv`.

## Dataset exports

`ScenarioSet.from_export` trains on either dataset format:

| Format | Where it comes from | A scenario becomes |
|---|---|---|
| `simforge.public-dataset/v1` | the public SimForge dataset (Hugging Face) | its `document.json` and the recordings it pins, compiled by the runtime |
| `simforge.dataset-export/v1` | your cloud export (`simforge datasets export`, `POST /api/v1/exports`) | its `scenario.zip` package; the episode is the input its simulation ran |

- `source` is a downloaded folder, or a cloud export id (`run_...`) that `simforge datasets pull ID --folder` downloads into the cache first (it resumes and checks every file).
- The rows of `split` (all with `split=None`), optionally only those on the map `map_name`, become workspace tasks, named by public id or scenario directory; identical copies appear once.
- Every file is checked against the digest its row (and the manifest's `SHA256SUMS` or `scenarios.jsonl` digest) records, and laid out once per scenario under `cache_dir` (default `$XDG_CACHE_HOME/simforge-rl`, else `~/.cache/simforge-rl`).
- An episode runs on the installed map whose OpenDRIVE digest the scenario names: `simforge maps pull` the maps your split uses.

Refusals are `ExportError(code, reason)`: `export_file_missing`, `export_schema_unsupported`, `export_digest_mismatch`, `export_rows_count`, `export_split_empty`, `export_row_unlisted`, `package_member_unsafe`, or the CLI's own codes when a pull fails. `read_export(folder)`, `pull_export(export_id, out)` and `export_scenarios(...)` are the pieces.

## Runtime protocol

### `RuntimeClient`

`RuntimeClient.spawn(binary=None, *args, env=None)` starts `simforge-runtime serve --stdio`: `binary`, else `$SIMFORGE_RUNTIME_BIN`, else the runtime `simforge runtime install` put under `$SIMFORGE_RUNTIME_ROOT` (default `~/.local/share/simforge/runtime`), else `simforge-runtime` on PATH. `RuntimeClient.connect(path=None)` attaches to a running `simforge-runtime serve --socket PATH` (`$SIMFORGE_RUNTIME_SOCKET`).

- Execution: `help`, `command`, `compile`, `simulate`, `transfer`, `evaluate`, `render(argv, mode)`, `preview(argv, mode)` (mode `2d`, `schematic` or `3d`), each returning `CommandResult(value, exit_code)`; exit code 2 means completed with findings.
- Episodes: `env_create/env_reset/env_step/env_observe/env_checkpoint/env_restore/env_close`, or `RuntimeEnvTransport(runtime, argv)`.
- Batches: `batch_create/batch_reset/batch_step/batch_observe/batch_close` (`RuntimeBatch`, `BatchResults`, `BatchRow`).
- Persistent worlds: `world_create/join/claim/command/subscribe/poll/unsubscribe/snapshot/checkpoint/restore/time/record/close` (`WorldSession`, `Subscription`, `TruthDelivery`, `WorldCheckpoint`).

A runtime refusal is a `RuntimeProtocolError` (an `EnvServeError` with `path`); malformed bytes raise `ProtocolError`.

## Serving helpers

- `EnvServer(workspace, socket, *args, binary=None, env=None)`: a running `simforge env serve`; raises `EnvServeError` with the server's refusal code if it exits before listening.
- `template_workspace(template, map_dir, out, *, map_id=None, xodr_sha256=None, site=None)`: a package workspace that `env serve` compiles `template` into, on `map_dir` or (with `map_dir=None`) on the installed map `map_id` with OpenDRIVE `xodr_sha256`; `site` pins the template's site.

Both start the runtime with cloud credentials (`SIMFORGE_TOKEN`, `SIMFORGE_API_KEY`, ...) removed from its environment.

## Rewards

- `native`: the runtime's reward, unchanged.
- `WeightedTerms(weights, *, bias=0.0)`: `bias + sum(weights[k] * info["reward_terms"][k])`; an unknown term is an error.
- `EventPenalty(kinds, base=native)`: subtracts `kinds[kind]` per engine event (`info["events"]`).

Both need a state observation mode with `info_channel=True`. `info["native_reward"]` always keeps the runtime's value.

## PPO example

`python -m simforge_rl.examples.ppo --help` lists every flag. Task sources (one of): `--tasks NAME` (bundled set, default `richmond-field-station`), `--scenario-set FILE`, `--export DIR|EXPORT_ID` with `--split` and `--export-map`, or `--endpoint [NAME=]PATH`. Sessions run through spawned `simforge env serve` sockets, or with `--runtime [BIN]` through `simforge-runtime serve` (no BIN: the installed runtime). `--max-open` caps sessions per sub-env and `--eval-limit` evaluates on the first N scenarios. `--out DIR` writes `curve.csv`, `curve.svg`, `metadata.json` and the policy.
