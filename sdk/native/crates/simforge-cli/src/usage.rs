//! What this CLI tells the installed runtime about the commands it runs, for
//! the usage reports of SimForge's account and download terms (draft 4,
//! section 3(a)(ii), (iii), (vi) and (vii)).
//!
//! The CLI sends nothing itself. Only when the runtime's sender has recorded
//! that the signed-in account's terms allow these records
//! (`~/.simforge/usage/terms.json` naming `simforge.usage-batch/v2`) does a
//! command leave one small file in `~/.simforge/usage/cli/`
//! (`simforge.cli-usage/v1`): the command's path and the names of the options
//! used, both taken from this CLI's own command surface (never a value, a
//! path or a word the user typed that the surface does not define), how long
//! it took, its outcome or error code, and which AI coding agent ran it
//! (`sdk/contracts/telemetry-agents.v1.json`). A crash leaves the panic's
//! source location instead; the runtime reduces it to a hash. The runtime
//! checks every file again and turns it into an event; `simforge telemetry
//! show` prints the result exactly as it will be sent.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

pub const SPOOL_SCHEMA: &str = "simforge.cli-usage/v1";
const V2: &str = "simforge.usage-batch/v2";
/// Files left for the runtime at most; more are not written until it imports them.
const MAX_SPOOL: usize = 2_000;
/// A command the surface does not define is recorded under this path.
const UNKNOWN: &str = "unknown";

#[derive(Deserialize)]
struct Agents {
    agents: Vec<AgentEntry>,
}

#[derive(Deserialize)]
struct AgentEntry {
    agent: String,
    markers: Vec<String>,
}

/// The agent whose marker `set` reports, else `none` (same table and order as the runtime).
pub fn agent_with(set: impl Fn(&str) -> bool) -> String {
    let table: Agents = serde_json::from_str(include_str!(
        "../../../../contracts/telemetry-agents.v1.json"
    ))
    .expect("telemetry-agents.v1.json is the agent table");
    table
        .agents
        .into_iter()
        .find(|entry| entry.markers.iter().any(|marker| set(marker)))
        .map_or_else(|| "none".to_owned(), |entry| entry.agent)
}

fn agent() -> String {
    agent_with(|marker| std::env::var_os(marker).is_some_and(|value| !value.is_empty()))
}

/// The command path and option names of `rest` (argv without the binary)
/// as `surface` (`help_surface()`) defines them. Every word returned is a
/// command or option name of the surface; everything else is dropped.
pub fn describe(surface: &Value, rest: &[String]) -> (String, Vec<String>) {
    let flag_list = |v: &Value| -> Vec<(String, bool)> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| Some((f["name"].as_str()?.to_owned(), f["takesValue"] == true)))
            .filter(|(name, _)| name.starts_with("--"))
            .collect()
    };
    let globals = flag_list(&surface["globalFlags"]);
    let options: Vec<&String> = rest.iter().take_while(|a| a.as_str() != "--").collect();
    let mut words = Vec::new();
    let mut skip = false;
    for token in &options {
        if std::mem::take(&mut skip) {
            continue;
        }
        if token.starts_with('-') {
            skip = !token.contains('=')
                && globals.iter().any(|(name, value)| *value && name == *token);
        } else if words.len() < 4 {
            words.push(token.as_str());
        }
    }
    let command = surface["commands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| {
            c["command"].as_str().is_some_and(|name| {
                let parts: Vec<&str> = name.split(' ').collect();
                words.len() >= parts.len() && words[..parts.len()] == parts[..]
            })
        })
        .max_by_key(|c| c["command"].as_str().map_or(0, |n| n.split(' ').count()));
    let path = command
        .and_then(|c| c["command"].as_str())
        .unwrap_or(UNKNOWN)
        .to_owned();
    let mut known: Vec<String> = globals.into_iter().map(|(name, _)| name).collect();
    if let Some(command) = command {
        known.extend(
            flag_list(&command["flags"])
                .into_iter()
                .map(|(name, _)| name),
        );
    }
    known.push("--help".into());
    let mut flags: Vec<String> = options
        .iter()
        .filter(|t| t.starts_with("--"))
        .map(|t| t.split('=').next().unwrap_or_default().to_owned())
        .filter(|name| known.contains(name))
        .collect();
    flags.sort();
    flags.dedup();
    (path, flags)
}

fn usage_dir() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join(".simforge").join("usage"))
}

/// The spool directory, when the account's terms allow these records.
fn spool() -> Option<PathBuf> {
    let dir = usage_dir()?;
    let terms: Value = serde_json::from_slice(&std::fs::read(dir.join("terms.json")).ok()?).ok()?;
    (terms["batchSchema"] == V2).then(|| dir.join("cli"))
}

fn write(dir: &Path, entry: &Value) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let waiting = std::fs::read_dir(dir).map(|entries| entries.take(MAX_SPOOL).count());
    if !matches!(waiting, Ok(n) if n < MAX_SPOOL) {
        return;
    }
    let mut id = [0u8; 16];
    if getrandom::fill(&mut id).is_err() {
        return;
    }
    let name: String = id.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = dir.join(format!(".tmp-{name}"));
    if std::fs::write(&tmp, entry.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(format!("{name}.json")));
    }
}

/// One CLI invocation in progress.
struct Run {
    rest: Vec<String>,
    started: Instant,
}

static CURRENT: Mutex<Option<Run>> = Mutex::new(None);

/// Start recording this invocation and record a crash of it.
pub fn start(rest: &[String]) {
    if let Ok(mut current) = CURRENT.lock() {
        *current = Some(Run {
            rest: rest.to_vec(),
            started: Instant::now(),
        });
    }
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(location) = info.location() {
                let panic = json!({"file": location.file(), "line": location.line()});
                end(Some(("error", Some("panic"))), Some(panic));
            }
            previous(info);
        }));
    });
}

fn end(outcome: Option<(&str, Option<&str>)>, panic: Option<Value>) {
    let Some(run) = CURRENT.try_lock().ok().and_then(|mut c| c.take()) else {
        return;
    };
    let Some(dir) = spool() else { return };
    let (outcome, error_code) = outcome.unwrap_or(("ok", None));
    let (command, flags) = describe(&crate::help_surface(), &run.rest);
    let finished_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let entry = json!({
        "schema": SPOOL_SCHEMA,
        "finishedAt": finished_at,
        "wallSeconds": (run.started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
        "command": command,
        "flags": flags,
        "outcome": outcome,
        "errorCode": error_code,
        "agent": agent(),
        "cliVersion": env!("CARGO_PKG_VERSION"),
        "panic": panic,
    });
    write(&dir, &entry);
}

/// Record the finished invocation: its exit code and, when it failed, its error code.
pub fn finish(exit: i32, error: Option<&str>) {
    let outcome = match (exit, error) {
        (_, Some(code)) => ("error", Some(code)),
        (0, None) => ("ok", None),
        (2, None) => ("findings", None),
        _ => ("error", Some("unclassified")),
    };
    end(Some(outcome), None);
}

/// This process is about to become the runtime (`env serve`, `world serve`):
/// the CLI's part ended well, and the runtime records the job itself.
pub fn hand_off() {
    end(None, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn a_command_is_its_surface_path_and_option_names_never_values() {
        let surface = crate::help_surface();
        let (path, flags) = describe(
            &surface,
            &argv(&[
                "maps",
                "pull",
                "yale-street",
                "--accept-license",
                "--pretty",
            ]),
        );
        assert_eq!(path, "maps pull");
        assert_eq!(flags, ["--accept-license", "--pretty"]);
        let (path, flags) = describe(
            &surface,
            &argv(&[
                "--workspace",
                "ws_secret",
                "rig",
                "init",
                "--preset=tesla-hw3",
                "--out",
                "/home/alice/rig.json",
            ]),
        );
        assert_eq!(path, "rig init");
        assert_eq!(flags, ["--out", "--preset", "--workspace"]);
    }

    #[test]
    fn hostile_arguments_yield_only_surface_words() {
        let surface = crate::help_surface();
        let mut vocabulary: Vec<String> = surface["commands"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["command"].as_str().map(str::to_owned))
            .collect();
        vocabulary.push(UNKNOWN.into());
        let mut names: Vec<String> = vec!["--help".into()];
        for list in std::iter::once(&surface["globalFlags"]).chain(
            surface["commands"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| &c["flags"]),
        ) {
            names.extend(
                list.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f["name"].as_str().map(str::to_owned)),
            );
        }
        let hostile = [
            "/home/alice/secret project/scenario.json",
            "--alice-secret-flag",
            "--out=/home/alice",
            "C:\\Users\\bob\\x.json",
            "-",
            "--",
            "render",
            "--preset",
            "sk-not-a-real-secret-value",
            "maps",
            "äöü",
            "--=",
            "pull",
            "",
        ];
        // Every arrangement of up to four hostile words.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..2_000 {
            let len = (state % 5) as usize;
            let words: Vec<String> = (0..len)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1);
                    hostile[(state >> 33) as usize % hostile.len()].to_owned()
                })
                .collect();
            state = state.wrapping_add(7);
            let (path, flags) = describe(&surface, &words);
            assert!(vocabulary.contains(&path), "{path:?} from {words:?}");
            for flag in &flags {
                assert!(names.contains(flag), "{flag} from {words:?}");
            }
        }
    }

    #[test]
    fn the_agent_comes_from_the_shared_marker_table() {
        assert_eq!(agent_with(|m| m == "CLAUDECODE"), "claude-code");
        assert_eq!(agent_with(|m| m == "CODEX_SANDBOX"), "codex");
        assert_eq!(agent_with(|m| m == "CURSOR_AGENT"), "cursor");
        assert_eq!(agent_with(|m| m == "OPENCODE"), "other-agent");
        assert_eq!(agent_with(|_| false), "none");
    }
}
