//! `--help` as data. An agent reads the command surface as JSON; `--pretty`
//! renders clap's text help of the same command. The surface is built from the
//! clap tree, so it cannot drift from what the parser accepts, and it needs no
//! network, no cache and no private code.

use clap::{Arg, ArgAction, Command};
use serde_json::{json, Value};

use crate::commands;
use crate::contract::exit_code_table;

/// Find the (sub)command named by the leading non-flag tokens of `argv`
/// (without the binary name). Unknown words stop the walk: `simforge maps
/// frobnicate --help` describes `maps`, and the unknown word is reported.
pub fn locate<'a>(
    root: &'a Command,
    argv: &[String],
) -> (&'a Command, Vec<String>, Option<String>) {
    let mut current = root;
    let mut path = Vec::new();
    let mut positionals = 0;
    let mut option_values = 0;
    for token in argv {
        if option_values > 0 {
            option_values -= 1;
            continue;
        }
        if token == "--" {
            break;
        }
        if token.starts_with('-') {
            let long = token
                .strip_prefix("--")
                .map(|name| name.split('=').next().unwrap_or(name));
            let short = if long.is_none() {
                token.strip_prefix('-').and_then(|name| name.chars().next())
            } else {
                None
            };
            let inline = token.contains('=') || (short.is_some() && token.chars().count() > 2);
            if let Some(arg) = current
                .get_arguments()
                .chain(root.get_arguments().filter(|arg| arg.is_global_set()))
                .find(|arg| {
                    long.is_some_and(|name| arg.get_long() == Some(name))
                        || short.is_some_and(|name| arg.get_short() == Some(name))
                })
            {
                if !inline && matches!(arg.get_action(), ArgAction::Set | ArgAction::Append) {
                    option_values = arg.get_num_args().map_or(1, |range| range.min_values());
                }
            }
            continue;
        }
        if token == "help" && path.is_empty() {
            continue;
        }
        match current.find_subcommand(token) {
            Some(sub) => {
                path.push(sub.get_name().to_owned());
                current = sub;
                positionals = 0;
            }
            None if current.get_positionals().nth(positionals).is_some() => {
                positionals += 1;
            }
            None if current.has_subcommands() => return (current, path, Some(token.clone())),
            // A positional value (e.g. `render ws --help`): stop at the leaf.
            None => break,
        }
    }
    (current, path, None)
}

/// Resolve runtime passthrough commands against the same merged registry that supplies their help.
pub fn locate_surface(
    root: &Command,
    surface: &Value,
    argv: &[String],
) -> (Vec<String>, Option<String>) {
    let (_, native_path, unknown) = locate(root, argv);
    if !native_path
        .first()
        .is_some_and(|name| commands::is_runtime_command(name))
    {
        return (native_path, unknown);
    }
    let entries = surface["commands"].as_array().expect("command surface");
    let mut path = Vec::new();
    let mut positionals = 0;
    let mut option_value = false;
    for token in argv {
        if option_value {
            option_value = false;
            continue;
        }
        if token == "--" {
            break;
        }
        let prefix = path.join(" ");
        let descendants: Vec<_> = entries
            .iter()
            .filter(|entry| {
                let name = entry["command"].as_str().unwrap_or("");
                prefix.is_empty() || name == prefix || name.starts_with(&format!("{prefix} "))
            })
            .collect();
        if token.starts_with('-') {
            let flag = token.split('=').next().unwrap_or(token);
            option_value = !token.contains('=')
                && surface["globalFlags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(descendants.iter().flat_map(|entry| {
                        entry["flags"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|arg| {
                                arg["commandIndex"]
                                    .as_u64()
                                    .is_some_and(|index| index <= path.len() as u64)
                            })
                    }))
                    .any(|arg| arg["name"] == flag && arg["takesValue"] == true);
            continue;
        }
        if token == "help" && path.is_empty() {
            continue;
        }
        let next = if prefix.is_empty() {
            token.clone()
        } else {
            format!("{prefix} {token}")
        };
        if descendants.iter().any(|entry| {
            let name = entry["command"].as_str().unwrap_or("");
            name == next || name.starts_with(&format!("{next} "))
        }) {
            path.push(token.clone());
            positionals = 0;
        } else if descendants.iter().any(|entry| {
            entry["arguments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|arg| arg["commandIndex"] == path.len())
                .nth(positionals)
                .is_some()
        }) {
            positionals += 1;
        } else if descendants.iter().any(|entry| entry["command"] != prefix) {
            return (path, Some(token.clone()));
        } else {
            break;
        }
    }
    (path, None)
}

/// The value type an argument parses to, for machine readers.
fn value_type(arg: &Arg, takes_value: bool) -> &'static str {
    use std::any::TypeId;
    if !takes_value && !arg.is_positional() {
        return "bool";
    }
    if !arg.get_possible_values().is_empty() {
        return "enum";
    }
    let t = arg.get_value_parser().type_id();
    if t == TypeId::of::<std::path::PathBuf>() {
        "path"
    } else if t == TypeId::of::<u64>()
        || t == TypeId::of::<u32>()
        || t == TypeId::of::<u16>()
        || t == TypeId::of::<usize>()
        || t == TypeId::of::<i64>()
        || t == TypeId::of::<i32>()
    {
        "integer"
    } else if t == TypeId::of::<f64>() || t == TypeId::of::<f32>() {
        "number"
    } else if t == TypeId::of::<bool>() {
        "bool"
    } else {
        "string"
    }
}

fn arg_json(arg: &Arg) -> Value {
    let takes_value = matches!(arg.get_action(), ArgAction::Set | ArgAction::Append);
    let possible: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| v.get_name().to_owned())
        .collect();
    let defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().into_owned())
        .collect();
    let mut out = serde_json::Map::new();
    if arg.is_positional() {
        out.insert("name".into(), json!(arg.get_id().as_str()));
    } else {
        out.insert(
            "name".into(),
            json!(format!(
                "--{}",
                arg.get_long().unwrap_or(arg.get_id().as_str())
            )),
        );
        out.insert("takesValue".into(), json!(takes_value));
    }
    if let Some(names) = arg.get_value_names() {
        if takes_value || arg.is_positional() {
            out.insert(
                "valueName".into(),
                json!(names
                    .iter()
                    .map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")),
            );
        }
    }
    out.insert("type".into(), json!(value_type(arg, takes_value)));
    out.insert("required".into(), json!(arg.is_required_set()));
    if matches!(arg.get_action(), ArgAction::Append) {
        out.insert("repeatable".into(), json!(true));
    }
    if !defaults.is_empty() && takes_value {
        out.insert("default".into(), json!(defaults.join(",")));
    }
    if !possible.is_empty() {
        out.insert("possibleValues".into(), json!(possible));
    }
    if arg.is_last_set() {
        out.insert("last".into(), json!(true));
    }
    if let Some(env) = arg.get_env() {
        out.insert("env".into(), json!(env.to_string_lossy()));
    }
    if arg.is_global_set() {
        out.insert("global".into(), json!(true));
    }
    out.insert(
        "help".into(),
        json!(arg.get_help().map(|h| h.to_string()).unwrap_or_default()),
    );
    Value::Object(out)
}

/// The flags (not positionals, not globals) a command declares, as the help documents them.
pub fn flags_of(cmd: &Command) -> Vec<Value> {
    cmd.get_arguments()
        .filter(|a| !a.is_positional() && !a.is_hide_set() && !a.is_global_set())
        .map(arg_json)
        .collect()
}

fn summary(cmd: &Command) -> String {
    cmd.get_about().map(|s| s.to_string()).unwrap_or_default()
}

fn status(path: &str) -> &'static str {
    if commands::PLANNED.contains(&path) {
        "planned"
    } else {
        "available"
    }
}

/// Replace the `status` of every command in a help document (`command`, and each
/// `commands[]` entry by `name` or `command`) that `resolve` reports for, adding
/// `statusReason`: the host's status of a cloud server command for this credential.
pub fn enrich(doc: &mut Value, resolve: &dyn Fn(&str) -> Option<(String, Value)>) {
    fn one(item: &mut Value, key: &str, resolve: &dyn Fn(&str) -> Option<(String, Value)>) {
        let Some(name) = item.get(key).and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        if item.get("status").is_none() || name.is_empty() {
            return;
        }
        if let Some((status, reason)) = resolve(&name) {
            item["status"] = json!(status);
            item["statusReason"] = reason;
        }
    }
    one(doc, "command", resolve);
    if let Some(items) = doc.get_mut("commands").and_then(Value::as_array_mut) {
        for item in items {
            one(item, "name", resolve);
            one(item, "command", resolve);
        }
    }
}

/// A command that runs on its own: a leaf, or a group whose subcommand is
/// optional (`simforge bench` runs the benchmark, `bench compare` diffs).
fn runnable(cmd: &Command) -> bool {
    !cmd.has_subcommands() || !cmd.is_subcommand_required_set()
}

/// Every runnable command as `{name, summary, status}`, depth first.
pub fn flatten(cmd: &Command, prefix: &str, out: &mut Vec<Value>) {
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
        let name = if prefix.is_empty() {
            sub.get_name().to_owned()
        } else {
            format!("{prefix} {}", sub.get_name())
        };
        if runnable(sub) {
            out.push(json!({ "name": name, "summary": summary(sub), "status": status(&name) }));
        }
        if sub.has_subcommands() {
            flatten(sub, &name, out);
        }
    }
}

/// The help document for one command (the root when `path` is empty).
pub fn document(root: &Command, cmd: &Command, path: &[String]) -> Value {
    let name = path.join(" ");
    let mut cmd_for_usage = cmd.clone();
    let rendered = cmd_for_usage.render_usage().to_string();
    let usage = rendered.trim_start_matches("Usage: ").trim().to_owned();

    let args: Vec<&Arg> = cmd.get_arguments().filter(|a| !a.is_hide_set()).collect();
    let positionals: Vec<Value> = args
        .iter()
        .filter(|a| a.is_positional())
        .map(|a| arg_json(a))
        .collect();
    let flags: Vec<Value> = args
        .iter()
        .filter(|a| !a.is_positional() && !a.is_global_set())
        .map(|a| arg_json(a))
        .collect();
    let mut globals: Vec<Value> = root
        .get_arguments()
        .filter(|a| a.is_global_set())
        .map(arg_json)
        .collect();
    globals.push(json!({"name":"--help","takesValue":false,"type":"bool","required":false,"global":true,"help":"Print machine-readable command help."}));

    let mut doc = serde_json::Map::new();
    doc.insert("bin".into(), json!("simforge"));
    doc.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    doc.insert("command".into(), json!(name));
    doc.insert("summary".into(), json!(summary(cmd)));
    if let Some(long) = cmd.get_long_about() {
        doc.insert("description".into(), json!(long.to_string()));
    }
    if cmd.has_subcommands() {
        let mut commands = Vec::new();
        flatten(cmd, &name, &mut commands);
        doc.insert("commands".into(), json!(commands));
    }
    if !path.is_empty() && runnable(cmd) {
        doc.insert("status".into(), json!(status(&name)));
        doc.insert("usage".into(), json!(usage));
        doc.insert("arguments".into(), json!(positionals));
        doc.insert("flags".into(), json!(flags));
    } else if !flags.is_empty() {
        doc.insert("flags".into(), json!(flags));
    }
    doc.insert("globalFlags".into(), json!(globals));
    doc.insert("exitCodes".into(), exit_code_table());
    doc.insert(
        "output".into(),
        json!("stdout: one JSON document (indented with --pretty); stderr: {code, path?, reason, detail?} on failure"),
    );
    Value::Object(doc)
}

/// clap's text help for `--help --pretty`.
pub fn text(cmd: &Command, path: &[String]) -> String {
    let mut cmd = cmd.clone().bin_name(if path.is_empty() {
        "simforge".to_owned()
    } else {
        format!("simforge {}", path.join(" "))
    });
    cmd.render_long_help().to_string()
}

/// The whole command surface under `cmd` (`simforge.cli-surface/v1`): every
/// runnable command with its arguments and flags (type, value name, default,
/// possible values, required, repeatable, env), for skill generators and the
/// gate that checks skills against the binary.
pub fn surface(root: &Command, cmd: &Command, path: &[String]) -> Value {
    fn walk(
        root: &Command,
        cmd: &Command,
        path: &mut Vec<String>,
        inherited: &[Value],
        out: &mut Vec<Value>,
    ) {
        let mut arguments = inherited.to_vec();
        for arg in cmd
            .get_arguments()
            .filter(|a| !path.is_empty() && !a.is_hide_set() && !a.is_global_set())
        {
            let mut value = arg_json(arg);
            value["commandIndex"] = json!(path.len());
            value["positional"] = json!(arg.is_positional());
            arguments.push(value);
        }
        if !path.is_empty() && runnable(cmd) {
            let mut doc = document(root, cmd, path);
            doc["arguments"] = json!(arguments
                .iter()
                .filter(|a| a["positional"] == true)
                .collect::<Vec<_>>());
            doc["flags"] = json!(arguments
                .iter()
                .filter(|a| a["positional"] != true)
                .collect::<Vec<_>>());
            if let Value::Object(map) = &mut doc {
                for key in [
                    "bin",
                    "version",
                    "globalFlags",
                    "exitCodes",
                    "output",
                    "commands",
                ] {
                    map.remove(key);
                }
            }
            out.push(doc);
        }
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
            path.push(sub.get_name().to_owned());
            walk(root, sub, path, &arguments, out);
            path.pop();
        }
    }
    let mut commands = Vec::new();
    let mut at = path.to_vec();
    walk(root, cmd, &mut at, &[], &mut commands);
    let root_doc = document(root, root, &[]);
    json!({
        "schema": "simforge.cli-surface/v1",
        "bin": "simforge",
        "version": env!("CARGO_PKG_VERSION"),
        "command": path.join(" "),
        "globalFlags": root_doc["globalFlags"],
        "exitCodes": root_doc["exitCodes"],
        "output": root_doc["output"],
        "commands": commands,
    })
}
