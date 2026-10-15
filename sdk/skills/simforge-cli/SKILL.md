---
name: simforge-cli
description: Drive the one `simforge` command line from an agent or over MCP. Its JSON stdout / structured stderr / exit 0-1-2 contract, reading the machine help (`simforge help --json`), the local-runtime default versus the global `--cloud` flag, signing in, cost caps, dataset export and `simforge mcp`. Use whenever invoking simforge, parsing its output or errors, or wiring it into an agent as tools.
---

# simforge CLI

`simforge` is one open binary for every SimForge task. Without `--cloud` a
command runs on this machine's local runtime (`simforge runtime install`);
with the global `--cloud` flag the **same command** runs on the hosted API of
the host you signed in to. Nothing falls back from one to the other.

## The contract

- stdout: exactly one JSON document (`--json` is the default; `--pretty`
  indents the same document).
- stderr on failure: one JSON line `{code, path?, reason, hint, detail?}`.
  Key repair on `code` and follow `hint`; never parse `reason`.
- Event lines (`{event, ...}`, never a `code`) precede it only where a person
  or a progress bar needs them: `login`, `maps pull --progress`.

| Exit | Meaning | What to do |
|---|---|---|
| 0 | ok | read stdout |
| 1 | could not run (bad flags, missing file, runtime or map not installed, not signed in) | fix the invocation or install what `hint` names |
| 2 | ran and found something wrong with the input (refused operation, failed gate, digest mismatch) | report or repair the input; do not retry blindly |

`simforge help --json` prints the whole command surface: every command, flag,
type, default and allowed value, including the runtime's commands when it is
installed. `simforge <command> --help` prints one command. Unknown flags are
errors (`unknown_flag`), except after the scenario of `render`, `simulate` and
`preview`, whose trailing arguments are passed to the runtime: read the help
instead of guessing.

## Local or cloud

```sh
simforge render s.json --mode 2d --out out                                   # local runtime
simforge render s.json --mode 2d --out out --cloud --max-cost <cents> --wait --download   # hosted
simforge render s.json --mode 2d --out out --cloud --dry-run                               # price only
```

- `--dry-run` on a file first saves it to the workspace as an immutable
  revision (free) and prices that; the run that follows reuses it.

- `--cloud` needs a login and an explicit `--max-cost` (`max_cost_required`).
  A run whose estimate is above it is refused before anything starts.
- `--wait` follows the run to a terminal state; `--download` then writes its
  artifacts (sha256-checked) into `--out`.
- `--idempotency-key` makes a lost submission safe to resend: the host answers
  with the original run and charges once.
- Hosted bookkeeping commands are always hosted and take no `--cloud`:
  `runs`, `results`, `scenarios`, `datasets`, `drafts`, `authoring`, `usage`,
  `keys`.
- `transfer` and `import log` also run with `--cloud` (see `simforge-cloud`).

## Signing in

```sh
simforge auth status                 # exit 1 not_logged_in / session_invalid: sign in
simforge login                       # browser approval
simforge login --device              # no browser here (SSH, CI): a URL + code
simforge whoami
simforge logout
```

- `login` waits on a person: relay the `login.browser` / `login.device` event
  (`verificationUriComplete`, `userCode`). Never guess or retry an approval.
- `--host HOST` (or `SIMFORGE_HOST`) targets another deployment;
  without either, commands use the host of the last `login`, then
  `simforge.ai`. `SIMFORGE_TOKEN` overrides the stored login for CI.
- A login acts in the workspace of the organization chosen at approval;
  `--workspace` (or `SIMFORGE_WORKSPACE_ID`) only confirms it
  (`workspace_mismatch` otherwise). Sign in again to switch.
- Never print tokens; `auth status` names the store, never the value.

## Exporting a dataset (hosted)

```sh
simforge datasets export <dataset-id> --out ./export
simforge datasets status <export-id>
simforge datasets pull <export-id> --out ./export
simforge datasets verify ./export
```

Downloads resume and every file's sha256 is checked. Scenarios that could not
be packaged are left out and listed (an `export_scenarios_excluded` warning):
`simforge datasets retry <export-id>` packages them again.

## MCP

```sh
simforge mcp                         # stdio MCP server; tools list works before sign-in
simforge mcp --cwd /abs/project --only render --only preview --only scenario
```

- One tool per command of `simforge help --json` (`render`, `preview_frame`,
  `scenario_apply`, `maps_pull`, `runs_watch`, ...) with the command's
  arguments and flags as typed inputs, plus `simforge` taking a raw `args`
  array for chained operations (`scenario apply s.json add-actor ... -- set-ego ...`).
- Every call runs this same binary with the exact argv in an explicit `cwd`
  (absolute; default the server's `--cwd`) and returns
  `{argv, cwd, exitCode, ok, result, error, events}` as structured content;
  `isError` is true for exit 1 or 2. Local is the default; set `cloud: true`
  and `max-cost` on a tool call to run it hosted.
- `login` is a device-flow tool: relay its `notifications/message` approval URL/code to the human while it waits, then inspect the returned CLI receipt.
- `env serve` and `mcp` run until stopped and are started in a terminal, not in a finite tool call.

## Reference

<!-- simforge:reference auth status, whoami, mcp, skills install -->
_Generated from `simforge --help` (CLI 0.2.0-rc.4) by scripts/skills/skills.py; do not edit._

#### `simforge auth status`

The account, organization, scopes, expiry, host and credential store of this machine's login, checked with the host

Usage: `simforge auth status [flags]`

| Argument / flag | | What |
|---|---|---|
| `--host HOST` |  | SimForge host. Default: SIMFORGE_HOST, then the host of the last login, then simforge.ai |

#### `simforge whoami`

The signed-in account, organization, workspace and scopes

Usage: `simforge whoami [flags]`

| Argument / flag | | What |
|---|---|---|
| `--host HOST` |  | SimForge host. Default: SIMFORGE_HOST, then the host of the last login, then simforge.ai |

#### `simforge mcp`

Expose this command surface as MCP tools over stdio

Usage: `simforge mcp [flags]`

| Argument / flag | | What |
|---|---|---|
| `--cwd DIR` |  | Working directory for tool calls that do not pass `cwd` (default: this process's) |
| `--only COMMAND` | repeatable | Expose only the commands under these command paths (repeatable, e.g. `--only render --only "scenario apply"`) |

#### `simforge skills install`

Copy the bundled skills into an agent's skill directory

Usage: `simforge skills install [flags]`

| Argument / flag | | What |
|---|---|---|
| `--claude` |  | Install into ~/.claude/skills (Claude Code) |
| `--codex` |  | Install into $CODEX_HOME/skills, else ~/.codex/skills (Codex) |
| `--dir PATH` |  | Install into this directory |
| `--force` |  | Overwrite files that differ from what an earlier install wrote |

Every command also takes the global flags `--pretty`, `--json`, `--cloud`, `--workspace`, `--idempotency-key`, `--dry-run`, `--api-url`, `--map-version`, `--help`.

Exit codes: `0` ok; `1` command error: the command could not run; `2` findings: the command ran and found something wrong with its input.
<!-- /simforge:reference -->
