---
name: simforge-quickstart
description: The five-step SimForge agent flow with the one `simforge` CLI. Install the CLI and its local runtime, sign in and download a map, author a scenario JSON with typed operations, preview and render it in 2d, schematic or 3d (locally by default, or the same command with --cloud, a cost cap, --wait and --download), and optionally serve it to an RL policy. Use for any first SimForge task or when unsure which command comes next.
---

# SimForge in five steps

`simforge` is one open CLI. Commands run on this machine's **local runtime**
by default; the same command with the global `--cloud` flag runs on SimForge's
hosted API (`/api/v1`) instead. There is never an automatic fallback between
the two: a missing runtime or a cloud refusal is an error that names the fix.

Every command prints one JSON document on stdout (`--json` is the default and
may be passed explicitly). A failure prints `{code, path?, reason, hint,
detail?}` on stderr. Exit `0` ok, `1` could not run (fix the invocation or
the prerequisite `hint` names), `2` ran and found something wrong with the
input (report it; never retry blindly). `simforge help --json` prints the whole
command surface; `simforge <command> --help` prints one command's flags.

## 1. Install the CLI and the runtime

Install the CLI with the host's installer. It checks the archive's and the
binary's sha256 and installs only the open CLI, a single Linux x86_64 binary
in `~/.local/bin` (`SIMFORGE_INSTALL_DIR` overrides); Windows and macOS are not
supported:

```sh
curl -fsSL https://simforge.ai/install.sh | sh
```

`simforge help --json` must list `maps pull`, `doctor` and `env serve`.

```sh
simforge doctor                      # local CLI: what is installed, what is missing, and the fix for each
simforge runtime status              # the local runtime version and protocol identity
simforge runtime license             # read the runtime license and SimForge terms first
simforge runtime install --accept-license
simforge skills install --claude     # or --codex, or --dir PATH
```

The CLI itself is open and engine-free. The runtime (engine, renderer) is a
separate download for your account, so `runtime license` and `runtime
install` need a signed-in login (step 2: sign in first) and accepted terms:
the runtime's license, PolyForm Noncommercial 1.0.0, together with SimForge's
account and download terms (sign-in, usage reporting, the map pack license).
Accepting them is what entitles your workspace. Every download is
signed: the CLI refuses an unsigned or untrusted one (`signature_*`) before it
fetches a byte. Without a runtime, local commands fail with
`runtime_not_installed`; `--cloud` commands still work. 3D and schematic
rendering need an NVIDIA GPU with at least 8 GiB and driver 570 or newer on
Linux x86_64; on other hardware they fail with `gpu_unsupported`,
`gpu_memory_unsupported` or `gpu_driver_unsupported` (2D, simulation and
`env serve --no-sensors` need no GPU).

## 2. Sign in and download a map

```sh
simforge login                       # browser approval; `--device` prints a URL + code instead
simforge whoami                      # account, organization, workspace, scopes
simforge maps pull <map>@<version> --accept-license
```

- `login` waits on a person: relay the `login.browser` / `login.device`
  stderr event to the user. Never print tokens.
- `login --host HOST` signs in to another deployment and makes it
  the host of every later command (`--host` or `SIMFORGE_HOST` still
  overrides it); `logout` forgets it.
- Maps are downloaded only for a signed-in account that accepted the
  agreement; the first download of a public map or runtime entitles the
  account on the spot. Downloads are signed, content-addressed and verified by
  sha256; `--cache-root DIR` overrides the cache. An unentitled map fails with
  a named code: `entitlement_required` (a staff-only map) means pick another
  map, not retry, and `entitlement_revoked` means an operator withdrew it. The
  public maps are `richmond-field-station`, `easterbrook-discovery-school`,
  `el-camino-road`, `belmont-research-center`, `yale-street`,
  `san-ramon-phase-1` and `di-rosa-sf` (5–19 GB each); `NAME@VERSION` pins
  one version. The runtime is about 2.7 GB. Neither download prints progress,
  so allow tens of minutes; pull a version once, because pulling it again
  downloads it again.

## 3. Author the scenario JSON

Never hand-write the scenario document: create it on the installed map and
change it with typed authoring operations (`simforge-authoring` skill).

```sh
simforge locations find --map <map> --limit 20     # real lanes/junctions: roadAnchor.rsl and .s
simforge scenario actors                           # catalogue ids such as vehicle.sedan
simforge scenario new --map <map> --out s.json
simforge scenario apply s.json add-actor --id ego --catalog-id vehicle.sedan --lane-rsl <road>:<section>:<lane> --s 20 \
  -- set-ego --role-id ego -- set-clip --clip-seconds 20
simforge scenario check s.json --strictness warnings --preview
```

`<map>` is the installed map's name (`easterbrook-discovery-school`), not
`name@version`; the installed release is the one that is pinned.

## 4. Preview and render: 2d, schematic or 3d

```sh
simforge rig init --preset basic-dash-camera --out rig.json   # its camera is dashcam-front
simforge preview frame s.json --mode 2d --at 5 --out plan.png
simforge preview frame s.json --mode 3d --rig rig.json --camera dashcam-front --at 5 --out frame.png
simforge render s.json --mode 2d --out out-2d
simforge render s.json --mode schematic --rig rig.json --out out-schematic
simforge render s.json --mode 3d --rig rig.json --out out-3d
```

Before a schematic or 3d render, check the machine with `simforge doctor`
(its `gpu` check) and the runtime with `simforge runtime status`
(`capabilities.renderModes`).

The same commands run hosted with `--cloud`, an explicit cost cap and, to get
the files, `--wait --download`:

```sh
simforge render s.json --mode 3d --rig rig.json --out out-3d --cloud --dry-run   # price only
simforge render s.json --mode 3d --rig rig.json --out out-3d --cloud --max-cost <cents> --wait --download
```

`--max-cost` is required with `--cloud` (`max_cost_required`); never raise it
above the budget you were given. Details: `simforge-render`, `simforge-cloud`.

A `--dry-run` on a file saves it to the workspace as an immutable revision
(free) and prices that; the run that follows reuses exactly that revision.

## 5. Bonus: closed loop and RL

```sh
simforge env serve <workspace> --socket /tmp/sf.sock --no-sensors
```

Drive it locally with SimForge Gym, the open `simforge-rl` Gymnasium client (`simforge-closed-loop` skill).

For a persistent shared world rather than isolated RL episodes, create a private token file first (`umask 077; head -c 32 /dev/urandom | xxd -p > world.token`), then start the
local host:

```sh
simforge world serve --map <map> --scenario s.json --listen 127.0.0.1:8765 --token-file world.token --journal-dir world-journal
```

The ready JSON names token-free truth and command endpoints. Authenticate clients
with the token file; never put its contents in URLs, prompts or logs.
Browser clients require explicit repeatable `--allow-origin <http(s)-origin>`;
no browser origin is enabled by default. Native clients without an Origin header
still authenticate with the same private token.

## Reference

<!-- simforge:reference doctor, runtime status, runtime install, login, maps pull -->
_Generated from `simforge --help` (CLI 0.2.0-rc.4) by scripts/skills/skills.py; do not edit._

#### `simforge doctor`

Check this machine: GPU adapter, ffmpeg, cache roots, disk, registry reachability

Usage: `simforge doctor [flags]`

| Argument / flag | | What |
|---|---|---|
| `--offline` |  | Do not probe the registry (the check is reported as skipped, never as ok) |
| `--registry URL` |  | Registry to probe. Default: SIMFORGE_MAPS_REGISTRY, then the staff map registry (AWS credentials required) |
| `--timeout SECONDS` | default `10` | Network timeout for the registry probe |

#### `simforge runtime status`

Report the installed runtime's protocol and engine identity

Usage: `simforge runtime status [flags]`

#### `simforge runtime install`

Download and install the closed runtime (PolyForm Noncommercial 1.0.0) after accepting its license and SimForge's account and download terms

Usage: `simforge runtime install [flags]`

| Argument / flag | | What |
|---|---|---|
| `--target TARGET` | default `x86_64-unknown-linux-gnu` |  |
| `--version VERSION` |  |  |
| `--root ROOT` |  |  |
| `--accept-license` |  | Explicitly accept the current license and terms (agreement version and SHA) before downloading |

#### `simforge login`

Sign in to a SimForge host (browser approval, or --device for a URL and code)

Usage: `simforge login [flags]`

| Argument / flag | | What |
|---|---|---|
| `--host HOST` |  | SimForge host to sign in to. Default: SIMFORGE_HOST, then the host of the last login, then simforge.ai. Later commands use the host signed in to |
| `--device` |  | Sign in with a code instead of a local browser: print a URL and a short code to approve from any browser (SSH, headless machines) |
| `--scope SCOPE` | repeatable | A scope to request (repeatable). Defaults cover map/runtime downloads and scenario/cloud jobs |
| `--credential-store STORE` | one of `auto`, `keyring`, `file` | Where to keep the tokens. Default: SIMFORGE_CREDENTIAL_STORE, then auto (the OS keychain, else a 0600 file; the output says which and why) |
| `--device-name NAME` |  | The name the approval page and the CLI sessions list show for this machine. Default: the host name |
| `--timeout SECONDS` | default `600` | Seconds to wait for the approval |

#### `simforge maps pull`

Download a map your signed-in account is entitled to into the local cache, sha256-verified

Usage: `simforge maps pull <map> [flags]`

| Argument / flag | | What |
|---|---|---|
| `<map>` | required | SimForge-owned map name, optionally pinned as NAME@VERSION |
| `--cache-root CACHE_ROOT` |  | Map cache directory. Default: the SimForge maps cache |
| `--accept-license` |  | Explicitly accept the current license and terms, which include the map pack terms, before downloading |

Every command also takes the global flags `--pretty`, `--json`, `--cloud`, `--workspace`, `--idempotency-key`, `--dry-run`, `--api-url`, `--map-version`, `--help`.

Exit codes: `0` ok; `1` command error: the command could not run; `2` findings: the command ran and found something wrong with its input.
<!-- /simforge:reference -->
