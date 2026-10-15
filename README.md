# SimForge toolkit

The Apache-2.0 toolkit contains the SimForge command-line front end, Python protocol clients, agent skills, scenario contracts and a Gymnasium reinforcement-learning package. The simulation engine, compiler, transfer implementation and Helios renderer are a separate proprietary runtime; this tree neither contains nor builds them. Map and actor packs are not source-code assets and have their own distribution terms.

Sign in at [simforge.ai](https://simforge.ai) to download the runtime (`simforge runtime install`) and the released maps (`simforge maps pull`); the runtime is licensed under the [PolyForm Noncommercial License 1.0.0](https://polyformproject.org/licenses/noncommercial/1.0.0), and downloads and the maps are covered by the SimForge account and download terms (sign-in, usage telemetry, maps free for non-commercial use without redistribution). The CLI shows both before you accept them.

## Quickstart

Linux x86_64 (Ubuntu 24.04 or later); 3D rendering needs an NVIDIA GPU with 8 GiB or more. The full walk-through is at [simforge.ai/docs/quickstart](https://simforge.ai/docs/quickstart).

```sh
curl -fsSL https://simforge.ai/install.sh | sh             # the open CLI
simforge login --device                                    # approve the device code in your browser
simforge runtime install --accept-license                  # read `simforge runtime license` first
simforge render scenario.json --mode 3d --out frames       # after `simforge maps pull <map> --accept-license`
```

## Build the command-line front end

```sh
cargo build --release --manifest-path sdk/Cargo.toml -p simforge --bin simforge
./sdk/target/release/simforge --help
```

The front end delegates local execution to an installed closed runtime through the versioned protocol in `sdk/contracts/runtime-protocol.v1.json`. Cloud execution uses `/api/v1`; it requires authentication. No engine or renderer source is required to build this front end.

## Local or `--cloud`

Everything runs locally by default and costs nothing. Add `--cloud --max-cost CENTS` to the same command to run it on SimCloud instead, when your machine cannot do it:

| You want | Locally | With `--cloud` |
|---|---|---|
| Photoreal camera/lidar renders (Helios) | needs an NVIDIA GPU with 8 GiB or more (texture tiers: simforge.ai/docs/render) | `simforge render scenario.json --mode 3d --cloud --max-cost N --out DIR`, any GPU or none |
| A 2D top-down video | `simforge render scenario.json --mode 2d` | the same plus `--cloud --max-cost N` |
| Your own driving logs as scenarios | not available | `simforge import log drive.recording.json --map auto --cloud --max-cost N`; a ROS 2 MCAP lidar log is perceived first (`--lidar-topic`, `--gnss-topic`) |
| The same event on another map | `simforge transfer scenario.json --map NAME@vN --out DIR` | the same plus `--cloud --max-cost N` |
| A training set from your scenarios | not available | `simforge datasets export DATASET --out DIR`, in the public dataset format |
| RL training | `simforge-rl`, local only | not offered |

- **Cost.** Every cloud command takes `--dry-run`, which prints the price and a per-job breakdown without starting anything. `--max-cost` is required and refuses a run whose price is higher. Prices come from your workspace's credit; a new workspace starts with free credit. `simforge usage` shows what you spent and your balance.
- **Limits.** A workspace has a daily spending cap. A refused run says which limit it hit (`budget_exceeded`: your `--max-cost`, the daily cap, or your credit) and what to do.
- **Where it runs.** Cloud runs use only what you upload and the maps your account can use: the seven released maps for every signed-in account. Results are stored in your workspace, and `--out` downloads them.
- **Keys for automation.** `simforge keys create --name ci` makes a scoped API key for CI (`SIMFORGE_API_KEY`); `simforge keys list` and `simforge keys revoke ID` manage them. Studio has the same under Workspace settings, API keys.
- **Non-commercial perception.** Scenarios made from an MCAP log use a lidar detector whose weights are CC BY-NC-SA 4.0, so that output is for non-commercial use only. Recordings you upload already perceived carry no such restriction from us.

## Python

```sh
pip install simforge-cloud
pip install 'simforge-rl[ppo]'
```

Or from this tree: `pip install ./clients/python` and `pip install './clients/rl[ppo]'`.

See `clients/python/README.md` for local and cloud calls and `clients/rl/README.md` for Gymnasium environments, vector environments, seeded scenario distributions, rendered observations, reward hooks and the CPU PPO example. The example's measured learning curve and reproduction inputs are in `clients/rl/evidence/`.

## Source contributions

See [CONTRIBUTING.md](CONTRIBUTING.md). This mirror receives only one-way, allowlisted exports from the maintained source. It carries no private source history. Earlier versions released under Apache-2.0 retain their existing grants; this notice describes the current component boundary only.

Toolkit code is [Apache-2.0](LICENSE). Installing the toolkit does not grant a licence to the separate runtime or to maps and other downloaded content.
