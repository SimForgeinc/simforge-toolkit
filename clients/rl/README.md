# simforge-rl

Train driving policies in SimForge on your own machine. `simforge-rl` gives you Gymnasium environments over the SimForge runtime, task sets on the released SimForge maps, training on SimForge dataset exports, and a PPO example. Apache-2.0. Dependencies: `numpy` and `gymnasium`; the PPO example adds `torch` (`pip install 'simforge-rl[ppo]'`).

The episodes run in the SimForge runtime, a closed program you install after signing in; this package is the open client. It never imports runtime code and never substitutes another simulator: no runtime, no episode. Training runs locally; there is no cloud RL.

## Quickstart (CPU only)

State observations need no GPU. With the `simforge` CLI installed:

```sh
simforge login                                                 # a browser approves the device code
simforge runtime install --accept-license                      # the closed runtime, under ~/.local/share/simforge/runtime
simforge maps pull richmond-field-station@v9 --accept-license  # a released map
pip install 'simforge-rl[ppo]'
python -m simforge_rl.examples.ppo --tasks richmond-field-station --runtime --num-envs 8 --total-timesteps 200000 --out run/
```

`run/` then holds `curve.csv`, `curve.svg`, `metadata.json` and `policy.pt`. `--runtime` runs each vector worker's episodes in its own `simforge-runtime serve` process; without it the example starts `simforge env serve` processes through the CLI instead. Both give the same observations and rewards.

In Python:

```python
from simforge_rl import RuntimeSessions, ScenarioSet, make_vector_env

tasks = ScenarioSet.bundled("richmond-field-station")          # 24 tasks: 15 templates at verified sites
with RuntimeSessions(tasks, "--no-sensors", workdir="work") as sessions:
    envs = make_vector_env(tasks, sessions.transports(), num_envs=8)
    obs, info = envs.reset(seed=0)
    obs, reward, terminated, truncated, info = envs.step(envs.action_space.sample())   # [target_speed_mps, target_accel_mps2] per env
```

## Task sets on the released maps

`bundled_sets()` names the task sets shipped with the package: `belmont-research-center`, `easterbrook-discovery-school`, `el-camino-road`, `richmond-field-station` and `yale-street`, 24 to 29 tasks each. Each is a weighted set of scenario templates (car following, cut-ins, lead braking, crossing pedestrians and cyclists, unprotected turns, red-light runners, vehicles pulling out, ...), each at the sites of that map where it compiles and its authored episode runs, with every template weighted equally. A set is pinned to the OpenDRIVE digest of the map version it was verified on, so its episodes run on exactly that installed map: pull `NAME@VERSION` as the set's `map` says. [`evidence/task-sets/report.json`](evidence/task-sets/report.json) records every template, site and verification episode. Site ids come from the map pack's derived road data, so a republished pack comes with rebuilt sets (`scripts/build_task_sets.py`); a site the installed pack does not have fails with `no_site` rather than moving the task elsewhere.

Every actor in the templates is in the public CARLA asset pack.

## Train on a dataset export

```python
tasks = ScenarioSet.from_export("exports/my-dataset", split="train")   # a downloaded folder
tasks = ScenarioSet.from_export("run_01JA...", split="train")          # your cloud export, pulled with `simforge datasets pull`
```

```sh
python -m simforge_rl.examples.ppo --export exports/my-dataset --split train --runtime --max-open 16 --eval-limit 8 --out run/
```

Both dataset formats work: the public SimForge dataset (`simforge.public-dataset/v1`) and cloud exports (`simforge.dataset-export/v1`, `POST /api/v1/exports`). Every file is checked against its recorded digest and laid out once; an episode runs on the installed map the scenario's OpenDRIVE digest names. `--max-open` keeps at most that many sessions open per worker (least recently used close first), so a split of thousands of scenarios trains with a bounded number of sessions.

## Camera observations

`observation="sensors"` returns rendered camera passes keyed `"<sourceId>/<pass>"` (`rgb` uint8 HxWx4, `depth` float32 HxW) and nothing else: the policy sees no engine state, object ids, reward terms or events. Rendering needs a supported GPU and an installed map's 3D pack. `simforge_rl/examples/rigs/alpamayo-fisheye.car.rig.json` is an Alpamayo-style rig: a 120-degree f-theta (fisheye) front camera and a 30-degree front tele camera, 512x384. Its lens is a generic equidistant calibration, not one taken from a real vehicle.

```sh
simforge env serve ws --socket cam.sock --rig alpamayo-fisheye.car.rig.json --observation-profile sensors \
  --passes rgb,depth --actor-closure "$(basename ~/.local/share/simforge/runtime/share/actors/closures/*.json .json)"
```

```python
env = SimForgeEnv(SocketTransport("cam.sock"), observation="sensors")
obs, info = env.reset(options={"episode_seed": 1})
frame = obs["camera_front_wide_120fov/rgb"]          # (384, 512, 4) uint8
```

`ws` is a scenario workspace, for example `template_workspace(...)` of a bundled task. `simforge rig show alpamayo-fisheye.car.rig.json` reports each camera's field of view; the installed runtime plans the f-theta lens.

## Rewards

`reward_fn(native_reward, info) -> float` replaces the reward; `info["native_reward"]` keeps the runtime's. `WeightedTerms({"progress": 1.0, "proximity": 2.0, "comfort": 1.0})` reweights the runtime's reward terms and `EventPenalty({"collision": 10.0})` subtracts per engine event; both need a state observation mode.

## The PPO result

[`evidence/ppo-scenario-set`](evidence/ppo-scenario-set) is a recorded PPO run on the bundled Richmond Field Station set through the distributable runtime, with its learning curve, configuration and a same-seed repeat.

The policy sees pose-free ego state and the eight nearest object rows and outputs a speed and acceleration setpoint. Each evaluation also runs the scenarios' authored ego control on the same tasks as a baseline. `python -m simforge_rl.examples.ppo --help` lists every flag.

## More

- [API reference](docs/api.md): environments, task sets, exports, the runtime protocol client (episodes, CPU batches of up to 64 episodes, persistent worlds) and the PPO flags.
- [RL methodology](docs/methodology.md): what the environment simulates, how it is evaluated, and what has been measured.

## Development

```sh
uv run --project clients/rl --extra dev python -m pytest -q clients/rl/tests
python3 scripts/sdk/toolkit-release.py build --ref HEAD --out "$TMPDIR/wheels"   # reproducible wheels
```

`SIMFORGE_BIN` and `SIMFORGE_RL_TEST_MAP_DIR` (Richmond Field Station v9, in a directory named `richmond-field-station`) enable the live runtime tests; `SIMFORGE_RL_TEST_SENSOR_MAP` and `SIMFORGE_RL_TEST_ACTOR_CLOSURE` add the camera test. Without them those tests are skipped and say so. `scripts/build_task_sets.py` rebuilds the bundled task sets from installed map packs.
