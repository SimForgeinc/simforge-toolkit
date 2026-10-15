//! `simforge mcp`: the CLI's own command surface as MCP tools over stdio.
//!
//! Tools are generated from the machine-help surface (`simforge help --json`:
//! the typed command tree merged with the runtime's command help), so they
//! cannot drift from what the CLI accepts and listing them needs no network
//! and no sign-in. A call runs this same binary (`current_exe`) with the argv the
//! tool's arguments spell, in an explicit working directory, and returns its
//! stdout document, its stderr error and its exit code unchanged: local and
//! `--cloud` behaviour, cost caps, refusals and hints are the CLI's own.

use std::collections::HashSet;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use clap::Args;
use serde_json::{json, Map, Value};

use crate::contract::{CliError, CmdResult, Ctx, Outcome};

#[derive(Debug, Args)]
pub struct McpArgs {
    /// Working directory for tool calls that do not pass `cwd` (default: this process's).
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<PathBuf>,
    /// Expose only the commands under these command paths (repeatable, e.g. `--only render --only "scenario apply"`).
    #[arg(long, value_name = "COMMAND")]
    pub only: Vec<String>,
}

/// Commands an MCP call cannot run: they wait on a person, serve until stopped, or are this server.
const BLOCKED: &[(&str, &str)] = &[
    ("mcp", "this is the MCP server itself"),
    ("env serve", "env serve runs until stopped: start it in a terminal and connect a client to its socket"),
    ("world serve", "world serve runs until stopped: start it in a terminal and use its authenticated drive/state endpoints"),
];

/// Global flags the server owns: output shape and help are fixed for machine reading.
const HIDDEN_GLOBALS: &[&str] = &["--pretty", "--help", "--json", "--version"];

const RAW_TOOL: &str = "simforge";

fn blocked(command: &str) -> Option<&'static str> {
    BLOCKED
        .iter()
        .find(|(c, _)| *c == command)
        .map(|(_, why)| *why)
}

fn tool_name(command: &str) -> String {
    let name: String = command.split_whitespace().collect::<Vec<_>>().join("_");
    name.chars().take(64).collect()
}

/// The JSON schema of one argument or flag of the help surface.
fn prop_schema(arg: &Value) -> Value {
    let ty = arg["type"].as_str().unwrap_or("string");
    let mut s = match ty {
        "bool" => json!({"type": "boolean"}),
        "integer" => json!({"type": "integer"}),
        "number" => json!({"type": "number"}),
        "enum" => json!({"type": "string", "enum": arg["possibleValues"]}),
        _ => json!({"type": "string"}),
    };
    let mut help = arg["help"].as_str().unwrap_or_default().to_owned();
    if ty == "path" {
        help = format!("{help} (path; relative paths resolve against cwd)")
            .trim()
            .to_owned();
    }
    if let Some(d) = arg.get("default") {
        help = format!("{help} [default: {}]", d.as_str().unwrap_or_default())
            .trim()
            .to_owned();
    }
    if !help.is_empty() {
        s["description"] = json!(help);
    }
    if arg["repeatable"] == json!(true) {
        let description = s["description"].take();
        s = json!({"type": "array", "items": s, "description": description});
    }
    s
}

fn cwd_schema() -> Value {
    json!({"type": "string", "description": "absolute working directory for the call (default: the server's --cwd)"})
}

/// One CLI flag or positional as a tool input key.
#[derive(Debug, Clone)]
struct Param {
    key: String,
    flag: Option<String>,
    takes_value: bool,
    ty: String,
    repeatable: bool,
    command_index: Option<usize>,
    last: bool,
}

#[derive(Debug, Clone)]
struct Tool {
    command: String,
    params: Vec<Param>,
    definition: Value,
}

fn param(arg: &Value, positional: bool) -> Param {
    let name = arg["name"].as_str().unwrap_or_default();
    Param {
        key: name.trim_start_matches("--").to_owned(),
        flag: (!positional).then(|| name.to_owned()),
        takes_value: positional || arg["takesValue"] == json!(true),
        ty: arg["type"].as_str().unwrap_or("string").to_owned(),
        repeatable: arg["repeatable"] == json!(true),
        command_index: arg["commandIndex"].as_u64().map(|n| n as usize),
        last: arg["last"] == true,
    }
}

fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "argv": {"type": "array", "items": {"type": "string"}, "description": "the exact simforge argv that ran"},
            "cwd": {"type": "string"},
            "exitCode": {"type": "integer", "description": "0 ok; 1 could not run; 2 ran and found something wrong with its input"},
            "ok": {"type": "boolean"},
            "result": {"description": "the command's stdout JSON document (null when it printed none)"},
            "error": {"description": "the command's stderr error {code, path?, reason, hint, detail?} (null on success)"},
            "events": {"type": "array", "description": "stderr event lines ({event, ...}) written while it ran"},
            "stdout": {"type": "string", "description": "stdout when it was not one JSON document"},
            "stderr": {"type": "string", "description": "stderr lines that were not JSON"},
        },
        "required": ["argv", "cwd", "exitCode", "ok", "result", "error", "events"],
    })
}

/// Tools for every runnable command of the surface, plus the raw argv tool.
fn catalogue(surface: &Value, only: &[String]) -> (Vec<Tool>, Value) {
    let globals: Vec<Value> = surface["globalFlags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|g| !HIDDEN_GLOBALS.contains(&g["name"].as_str().unwrap_or_default()))
        .cloned()
        .collect();
    let wanted = |command: &str| {
        only.is_empty()
            || only.iter().any(|o| {
                let o = o.split_whitespace().collect::<Vec<_>>().join(" ");
                command == o || command.starts_with(&format!("{o} "))
            })
    };
    let mut tools = Vec::new();
    let mut names = HashSet::new();
    for cmd in surface["commands"].as_array().into_iter().flatten() {
        let command = cmd["command"].as_str().unwrap_or_default().to_owned();
        if command.is_empty()
            || blocked(&command).is_some()
            || cmd["status"] == json!("planned")
            || !wanted(&command)
        {
            continue;
        }
        let name = tool_name(&command);
        if !names.insert(name.clone()) {
            continue;
        }
        let mut params = Vec::new();
        let mut props = Map::new();
        let mut required = Vec::new();
        let args = cmd["arguments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| (a, true));
        let flags = cmd["flags"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| (a, false));
        for (arg, positional) in args.chain(flags).chain(globals.iter().map(|g| (g, false))) {
            let p = param(arg, positional);
            if p.key.is_empty() || p.key == "cwd" || props.contains_key(&p.key) {
                continue;
            }
            props.insert(p.key.clone(), prop_schema(arg));
            if arg["required"] == json!(true) {
                required.push(p.key.clone());
            }
            params.push(p);
        }
        props.insert("cwd".into(), cwd_schema());
        let summary = cmd["summary"].as_str().unwrap_or_default();
        let usage = cmd["usage"]
            .as_str()
            .filter(|u| !u.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("simforge {command}"));
        let definition = json!({
            "name": name,
            "title": format!("simforge {command}"),
            "description": format!("{summary} Runs `{usage}` and returns its JSON result, error and exit code."),
            "inputSchema": {"type": "object", "properties": props, "required": required, "additionalProperties": false},
            "outputSchema": output_schema(),
            "_meta": {"simforge/command": command},
        });
        tools.push(Tool {
            command,
            params,
            definition,
        });
    }
    let raw = json!({
        "name": RAW_TOOL,
        "title": "simforge <args...>",
        "description": "Run any simforge command by its argv (without the binary name), e.g. [\"scenario\",\"apply\",\"s.json\",\"add-actor\",\"--id\",\"ego\",\"--\",\"set-ego\",\"--role-id\",\"ego\"] to chain operations, or [\"help\",\"--json\"] for the whole surface. Same JSON result, error and exit code as a terminal.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "args": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "cwd": cwd_schema(),
            },
            "required": ["args"],
            "additionalProperties": false,
        },
        "outputSchema": output_schema(),
    });
    let mut list: Vec<Value> = tools.iter().map(|t| t.definition.clone()).collect();
    list.push(raw);
    (tools, json!({ "tools": list }))
}

fn bad(code: &str, key: &str, reason: String) -> CliError {
    CliError::new(code, reason).with_path(key.to_owned())
}

/// A scalar argument as the one string the CLI parses.
fn scalar(p: &Param, v: &Value) -> Result<String, CliError> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => {
            return Err(bad(
                "bad_value",
                &p.key,
                format!("{} must be a {}", p.key, p.ty),
            ))
        }
    };
    if p.flag.is_none() && s.starts_with('-') {
        return Err(bad(
            "bad_value",
            &p.key,
            format!("{} must not start with '-' (write ./{s} for a path)", p.key),
        ));
    }
    Ok(s)
}

/// The argv (after the binary name) a typed tool call spells.
fn argv_of(tool: &Tool, args: &Map<String, Value>) -> Result<Vec<String>, CliError> {
    for key in args.keys() {
        if key != "cwd" && !tool.params.iter().any(|p| &p.key == key) {
            return Err(bad(
                "unknown_argument",
                key,
                format!("simforge {} has no argument {key}", tool.command),
            ));
        }
    }
    let words: Vec<String> = tool.command.split_whitespace().map(str::to_owned).collect();
    let mut slots = vec![Vec::<String>::new(); words.len() + 1];
    let mut tails = Vec::new();
    for p in &tool.params {
        let Some(v) = args.get(&p.key).filter(|v| !v.is_null()) else {
            continue;
        };
        let values: Vec<&Value> = match v {
            Value::Array(items) if p.repeatable => items.iter().collect(),
            Value::Array(_) => {
                return Err(bad(
                    "bad_value",
                    &p.key,
                    format!("{} takes one value", p.key),
                ))
            }
            one => vec![one],
        };
        let index = p.command_index.unwrap_or(words.len()).min(words.len());
        let slot = &mut slots[index];
        if p.last {
            tails.push("--".to_owned());
            for value in values {
                tails.push(scalar(p, value)?);
            }
            continue;
        }
        for v in values {
            match &p.flag {
                None => slot.push(scalar(p, v)?),
                Some(flag) if !p.takes_value => match v {
                    Value::Bool(true) => slot.push(flag.clone()),
                    Value::Bool(false) => {}
                    _ => {
                        return Err(bad(
                            "bad_value",
                            &p.key,
                            format!("{} must be a boolean", p.key),
                        ))
                    }
                },
                Some(flag) => slot.push(format!("{flag}={}", scalar(p, v)?)),
            }
        }
    }
    let mut argv = Vec::new();
    for (index, slot) in slots.into_iter().enumerate() {
        argv.extend(slot);
        if let Some(word) = words.get(index) {
            argv.push(word.clone());
        }
    }
    argv.extend(tails);
    Ok(argv)
}

/// The server's own global flags, passed to calls that do not set them.
fn defaults(ctx: &Ctx, argv: &[String]) -> Vec<String> {
    let has = |flag: &str| {
        argv.iter()
            .take_while(|a| a.as_str() != "--")
            .any(|a| a == flag || a.starts_with(&format!("{flag}=")))
    };
    let mut out = Vec::new();
    if ctx.cloud && !has("--cloud") {
        out.push("--cloud".to_owned());
    }
    if let Some(w) = ctx.workspace.as_ref().filter(|_| !has("--workspace")) {
        out.push(format!("--workspace={w}"));
    }
    if let Some(u) = ctx.api_url.as_ref().filter(|_| !has("--api-url")) {
        out.push(format!("--api-url={u}"));
    }
    if let Some(version) = ctx.map_version.as_ref().filter(|_| !has("--map-version")) {
        out.push(format!("--map-version={version}"));
    }
    if ctx.dry_run && !has("--dry-run") {
        out.push("--dry-run".to_owned());
    }
    out
}

/// Insert the server defaults before a chained `--`, where global flags still parse.
fn with_defaults(mut argv: Vec<String>, extra: Vec<String>) -> Vec<String> {
    let at = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
    argv.splice(at..at, extra);
    argv
}

fn call_cwd(root: &Path, args: &Map<String, Value>) -> Result<PathBuf, CliError> {
    let Some(v) = args.get("cwd").filter(|v| !v.is_null()) else {
        return Ok(root.to_path_buf());
    };
    let dir = PathBuf::from(
        v.as_str()
            .ok_or_else(|| bad("bad_value", "cwd", "cwd must be a string".into()))?,
    );
    if !dir.is_absolute() {
        return Err(bad(
            "bad_value",
            "cwd",
            format!("cwd must be absolute: {}", dir.display()),
        ));
    }
    if !dir.is_dir() {
        return Err(bad(
            "cwd_not_found",
            "cwd",
            format!("cwd is not a directory: {}", dir.display()),
        ));
    }
    Ok(dir)
}

/// Match the command prefix, never positional filenames.
fn blocked_in(argv: &[String]) -> Option<(String, &'static str)> {
    let argv: Vec<std::ffi::OsString> = std::iter::once("simforge".into())
        .chain(argv.iter().map(std::ffi::OsString::from))
        .collect();
    let normalized = crate::normalized_argv(&argv);
    let rest: Vec<String> = normalized[1..]
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    let command = super::execution::operation_argv(&rest);
    BLOCKED.iter().find_map(|(name, why)| {
        command
            .iter()
            .map(String::as_str)
            .take(name.split_whitespace().count())
            .eq(name.split_whitespace())
            .then(|| ((*name).to_owned(), *why))
    })
}

/// Run this binary with `argv` in `cwd`; the CLI contract as one structured value.
fn exec(argv: &[String], cwd: &Path, log_level: usize) -> Result<Value, CliError> {
    let exe = std::env::current_exe()
        .map_err(|e| CliError::new("mcp_io", format!("cannot locate simforge: {e}")))?;
    let out = run_child(&exe, argv, cwd, log_level)
        .map_err(|e| CliError::new("mcp_io", format!("cannot run simforge: {e}")))?;
    let code = out.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let result = serde_json::from_str::<Value>(stdout.trim()).ok();
    let (mut events, mut error, mut other) = (Vec::new(), Value::Null, Vec::new());
    for line in stderr.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(v) if v.get("event").is_some() => events.push(v),
            Ok(v) if v.get("code").is_some() => error = v,
            _ => other.push(line),
        }
    }
    let mut doc = json!({
        "argv": std::iter::once("simforge".to_owned()).chain(argv.iter().cloned()).collect::<Vec<_>>(),
        "cwd": cwd.display().to_string(),
        "exitCode": code,
        "ok": code == 0,
        "result": result.clone().unwrap_or(Value::Null),
        "error": error,
        "events": events,
    });
    if result.is_none() && !stdout.trim().is_empty() {
        doc["stdout"] = json!(stdout);
    }
    if !other.is_empty() {
        doc["stderr"] = json!(other.join("\n"));
    }
    Ok(doc)
}

fn run_child(exe: &Path, argv: &[String], cwd: &Path, log_level: usize) -> io::Result<Output> {
    let root = crate::command_tree();
    let (_, path, _) = crate::help::locate(&root, argv);
    let login = path.first().map(String::as_str) == Some("login");
    let mut command = Command::new(exe);
    command.args(argv).current_dir(cwd).stdin(Stdio::null());
    if !login {
        return command.output();
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let events = std::thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut collected = Vec::new();
        let mut reader = io::BufReader::new(stderr);
        loop {
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            if let Ok(event) = serde_json::from_slice::<Value>(&line) {
                if event.get("event").is_some() && log_level <= 1 {
                    let notification = json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","logger":"simforge","data":event}});
                    let mut out = io::stdout().lock();
                    writeln!(out, "{notification}")?;
                    out.flush()?;
                }
            }
            collected.extend(line);
        }
        Ok(collected)
    });
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes)?;
    let status = child.wait()?;
    let stderr = events
        .join()
        .map_err(|_| io::Error::other("login event reader failed"))??;
    Ok(Output {
        status,
        stdout: bytes,
        stderr,
    })
}

/// The envelope of a call the server refused before running anything.
fn refused(argv: &[String], cwd: &Path, e: &CliError) -> Value {
    json!({
        "argv": std::iter::once("simforge".to_owned()).chain(argv.iter().cloned()).collect::<Vec<_>>(),
        "cwd": cwd.display().to_string(),
        "exitCode": e.exit.code(),
        "ok": false,
        "result": Value::Null,
        "error": e.to_json(),
        "events": [],
    })
}

fn tool_call(
    tools: &[Tool],
    root: &Path,
    ctx: &Ctx,
    name: &str,
    args: &Value,
    log_level: usize,
) -> Value {
    let empty = Map::new();
    let args = args.as_object().unwrap_or(&empty);
    let planned = || -> Result<(Vec<String>, PathBuf), CliError> {
        let cwd = call_cwd(root, args)?;
        let mut argv = if name == RAW_TOOL {
            let words = args
                .get("args")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or_else(|| bad("missing_argument", "args", "args is required".into()))?;
            if let Some(k) = args.keys().find(|k| *k != "args" && *k != "cwd") {
                return Err(bad(
                    "unknown_argument",
                    k,
                    format!("{RAW_TOOL} has no argument {k}"),
                ));
            }
            words
                .iter()
                .map(|w| {
                    w.as_str().map(str::to_owned).ok_or_else(|| {
                        bad(
                            "bad_value",
                            "args",
                            "every args item must be a string".into(),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let tool = tools
                .iter()
                .find(|t| t.definition["name"] == name)
                .ok_or_else(|| bad("unknown_tool", name, format!("unknown MCP tool {name}")))?;
            argv_of(tool, args)?
        };
        if let Some((command, why)) = blocked_in(&argv).filter(|_| name == RAW_TOOL) {
            return Err(CliError::new(
                "mcp_command_unavailable",
                format!("simforge {command} cannot run as an MCP tool"),
            )
            .with_path(command)
            .with_detail(json!({ "hint": why })));
        }
        let mut effective = ctx.clone();
        if args.get("cloud") == Some(&json!(false)) {
            effective.cloud = false;
        }
        if args.get("dry-run") == Some(&json!(false)) {
            effective.dry_run = false;
        }
        let command_tree = crate::command_tree();
        let (_, path, _) = crate::help::locate(&command_tree, &argv);
        if path.first().map(String::as_str) == Some("login") {
            if args.get("device") == Some(&json!(false)) {
                return Err(CliError::new(
                    "mcp_login_requires_device",
                    "MCP login requires device approval",
                )
                .with_detail(
                    json!({"hint":"Use device=true, or run browser login in a terminal."}),
                ));
            }
            if !argv.iter().any(|arg| arg == "--device") {
                argv.push("--device".into());
            }
        }
        let extra = defaults(&effective, &argv);
        Ok((with_defaults(argv, extra), cwd))
    };
    let doc = match planned() {
        Ok((argv, cwd)) => {
            exec(&argv, &cwd, log_level).unwrap_or_else(|e| refused(&argv, &cwd, &e))
        }
        Err(e) => refused(&[], root, &e),
    };
    let is_error = doc["ok"] != json!(true);
    json!({
        "content": [{"type": "text", "text": doc.to_string()}],
        "structuredContent": doc,
        "isError": is_error,
    })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

pub fn run(args: McpArgs, ctx: &Ctx) -> CmdResult {
    let root = match args.cwd {
        Some(dir) => dir,
        None => std::env::current_dir()
            .map_err(|e| CliError::new("mcp_io", format!("no working directory: {e}")))?,
    };
    if !root.is_dir() {
        return Err(CliError::new(
            "cwd_not_found",
            format!("--cwd is not a directory: {}", root.display()),
        )
        .with_path("--cwd"));
    }
    let root = root.canonicalize().unwrap_or(root);
    let surface = crate::help_surface();
    let (tools, list) = catalogue(&surface, &args.only);
    let stdin = io::stdin();
    let mut out = io::stdout();
    let mut log_level = 1;
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| CliError::new("mcp_io", e.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let resp = rpc_error(Value::Null, -32700, &e.to_string());
                writeln!(out, "{resp}").map_err(|e| CliError::new("mcp_io", e.to_string()))?;
                out.flush().ok();
                continue;
            }
        };
        let Some(id) = req.get("id").cloned() else {
            continue; // a notification
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let response = match method {
            "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
                "capabilities": {"tools": {"listChanged": false}, "logging": {}},
                "serverInfo": {"name": "simforge", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Every tool runs the same CLI in an explicit cwd. Local runtime is the default; cloud=true uses the hosted API with max-cost. Login uses device approval and streams its prompt in logging notifications.",
            }}),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "logging/setLevel" => {
                let levels = [
                    "debug",
                    "info",
                    "notice",
                    "warning",
                    "error",
                    "critical",
                    "alert",
                    "emergency",
                ];
                if let Some(level) = params["level"]
                    .as_str()
                    .and_then(|value| levels.iter().position(|known| *known == value))
                {
                    log_level = level;
                    json!({"jsonrpc":"2.0","id":id,"result":{}})
                } else {
                    rpc_error(id, -32602, "level must be a standard MCP logging level")
                }
            }
            "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": list}),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let call_args = params.get("arguments").cloned().unwrap_or(json!({}));
                json!({"jsonrpc": "2.0", "id": id, "result": tool_call(&tools, &root, ctx, name, &call_args, log_level)})
            }
            _ => rpc_error(id, -32601, &format!("method not found: {method}")),
        };
        writeln!(out, "{response}").map_err(|e| CliError::new("mcp_io", e.to_string()))?;
        out.flush().ok();
    }
    Ok(Outcome::already_emitted(crate::contract::Exit::Ok))
}
