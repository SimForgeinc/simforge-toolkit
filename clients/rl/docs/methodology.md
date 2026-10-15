# RL methodology

Content for the SimForge website's RL page. Every statement here is backed by a file in this repository; the numbers come from [`evidence/ppo-scenario-set`](../evidence/ppo-scenario-set) and [`evidence/task-sets`](../evidence/task-sets/report.json). Update them from those files, not by hand.

## What you train in

- **The SimForge runtime, on your machine.** `simforge-rl` is a Gymnasium interface to the same closed runtime program that SimForge's cloud workers run. Training is local; the runtime works offline once installed.
- **Closed-loop, reactive traffic.** Other road users react to the policy's car. Physics steps at 50 Hz and the policy decides at 10 Hz.
- **Released maps and authored tasks.** Five released maps (Belmont, Easterbrook, El Camino, Richmond Field Station, Yale Street) ship a task set of 24 to 29 tasks each: scenario templates (car following, cut-ins, lead braking, pedestrians and cyclists crossing, unprotected turns, red-light runners, cars pulling out) placed at the sites of that map where they compile and run. Every actor is from the public CARLA asset pack.
- **Your own data.** Any SimForge dataset export, the public dataset or your own cloud export, becomes a task set: each episode replays the scenario's simulated input on its map.
- **Many episodes on a CPU.** Vector environments run episodes in parallel worker processes, and one runtime batches up to 64 state-only episodes across CPU threads.

## What the policy sees and does

- **State or cameras.** State observations are the ego's speed, acceleration, heading, lane offset and route progress plus the nearest objects' range, bearing and closing speed. Camera observations are rendered images and depth from a sensor rig, for example an Alpamayo-style fisheye front camera; in camera mode the policy receives no engine state, object identities, reward terms or events.
- **Actions.** A speed and acceleration setpoint, or throttle, brake and steering.
- **Reward.** A transparent default (progress, proximity and comfort terms) that you can reweight or replace on your side; the runtime's own value is always reported alongside.

## Reproducibility

- **Deterministic.** The same seed gives the same training curve: a second PPO run with the same seed reproduced every evaluation and training return at all checkpoints the two runs shared, byte for byte.
- **Pinned inputs.** Each task set records the OpenDRIVE digest of the map version it was verified on; each run records the runtime version, task digests and package versions.

## A measured baseline

PPO from the package's example, on the bundled Richmond Field Station task set (24 tasks from 15 templates), through the distributable runtime on one 24-core CPU host:

| | |
|---|---|
| Environment steps | 1,228,800 (10,832 episodes) |
| Wall time | 38 minutes, CPU only, at the lowest scheduling priority on a shared machine |
| Mean evaluation return, held-out seeds | 0.79 before training, 2.74 after |
| Scenarios' own authored driving, same tasks | 0.71 |

The trained policy beats the authored driving on average and on 12 of the 24 tasks, not on every task: it still crashes in some (disabled-vehicle, one lead-braking and one parked-row task), which the per-task table in the evidence shows.

Camera episodes with the Alpamayo-style rig (two 512x384 cameras, RGB and depth) stepped at 23 per second on one RTX 5080, a single measurement on one machine.

## Do not claim

GPU training speedups, results of camera-based policies, multi-agent training, cloud RL, Windows or macOS support, or that policies trained in SimForge transfer to other simulators or to real vehicles.
