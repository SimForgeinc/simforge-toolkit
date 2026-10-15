---
name: simforge-troubleshoot
description: Diagnose SimForge CLI failures from `simforge doctor` output and the structured errors every command prints ({code, reason, detail} on stderr, exit 1 or 2), including GPU/Vulkan adapter problems, missing ffmpeg or sky assets, map or closure mismatches, and render gate failures. Use when a simforge command fails, exits non-zero, or behaves differently on another machine.
---

# Troubleshooting simforge

## Read the contract first

- stdout: one JSON document (the result). stderr on failure:
  `{"code", "path"?, "reason", "hint", "detail"?}`; follow `hint`.
- Exit `0` ok; `1` the command could not run (bad flags, I/O, missing input);
  `2` it ran and found something wrong with its input (findings).
- `simforge <command> --help` prints that command's flags as JSON (`--pretty`
  for text): check a flag there before guessing.

## Start with doctor

```sh
simforge doctor --pretty
```

Each check has `status` (`ok`, `warn`, `fail`, `skipped`), `detail`, and
usually `fix`. Checks: the runtime (installed, version), the login, the GPU
adapter (the one a render would choose), ffmpeg, the sky assets and the cache
roots (free disk). Flags: simforge-quickstart's reference section.

A local failure is never fixed by silently switching to the cloud, or the
reverse: fix the local prerequisite, or rerun the same command with `--cloud`
and an explicit `--max-cost` if the user wants a hosted run.

## Common codes

| Code | Meaning | Do |
|---|---|---|
| `bad_value` | a flag's value is invalid | read `path` (the flag) and the command's `--help` |
| `runtime_not_installed` | a local command without the runtime | `simforge runtime install --accept-license` (signed in), or rerun with `--cloud` |
| `not_logged_in` | `--cloud`, `maps pull` or `runtime install` without a login | `simforge login` |
| `max_cost_required` / `budget_exceeded` | `--cloud` without a cap, or the estimate is above it | `--dry-run` to price; pass a deliberate `--max-cost` |
| `workspace_invalid` | not an imported workspace | `simforge package import pkg --into ws` first |
| `map_not_found` / `unknown_map` | the scenario's map release is not installed | `simforge maps pull name@version` (signed in, entitled) |
| `map_invalid` | the installed map differs from the package's closure | pull the pinned release; `--allow-map-drift` only on purpose (recorded) |
| `closure_invalid` / `attribution_invalid` | the actor closure is incomplete or unlicensed | `simforge assets pull --closure <digest>` again |
| `sky_assets_missing` | the renderer's sky plates are absent | `simforge assets pull --only sky` |
| `ffmpeg_broken` | video requested but ffmpeg is missing or fails | install ffmpeg (`simforge doctor` names it) |
| `render_rig_invalid` | the rig document is wrong | validate it as `simforge.render-rig/v1` |
| `parity_rejected` / `timeline_rejected` | the rebuilt timeline or sampled poses disagree with the package | the package is from a newer engine or damaged: upgrade the CLI or re-export |
| `texture_tier_miss` / `gpu_memory_unmeasured` | texture staging or VRAM admission | `--textures bc7-512`, or pass `--vram-budget` |
| `render_failed` | the renderer failed | `detail` has the renderer's error; retry on another adapter |
| `episode_not_reset` / `episode_invalid` / `unknown_op` | env serve protocol misuse | reset before step; check the client version |
| `version_ahead` (package finding) | the package needs a newer CLI | upgrade simforge |

## GPU and adapters

- No adapter: doctor warns (`no GPU: simulation and 2D renders run on the CPU`);
  only schematic and 3D renders need one. Install the GPU's Vulkan driver
  (SimForge runs on Linux x86_64 only). On a headless Linux box without a GPU, install Mesa (`mesa-vulkan-drivers`)
  and pass `--allow-software-adapter` to `render`/`env serve`: the CLI never
  falls back to the CPU on its own.
- Different pixels on another machine are expected across GPU models and
  drivers; traces and timelines are identical everywhere.
