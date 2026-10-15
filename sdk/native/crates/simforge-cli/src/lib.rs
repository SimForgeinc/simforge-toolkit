//! `simforge`: the SimForge command line.
//!
//! The contract lives in [`contract`]: one JSON document on stdout, a
//! structured `{code, path?, reason, detail?}` error on stderr, exit 0/1/2.
//! `--help` on any command prints that command's surface as JSON.
//!
//! Adding a command: a module under `commands/` with a clap `Args` (or
//! `Subcommand`) type and a `run(args, ctx) -> CmdResult`, one variant in
//! [`commands::Command`] and one arm in [`commands::dispatch`].

pub mod auth;
pub mod authoring_ops;
pub mod commands;
pub mod contract;
pub mod gpu_support;
pub mod help;
pub mod limits;
pub mod net;
pub mod paths;
pub mod rig;
pub mod runtime;
pub mod usage;

use std::ffi::OsString;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{CommandFactory, Parser};
use serde_json::json;

use contract::{emit, emit_error, CliError, Ctx, Exit};

#[derive(Debug, Parser)]
#[command(
    name = "simforge",
    about = "SimForge: verify scenario packages, pull maps and assets, render, re-simulate, and serve closed-loop episodes",
    disable_help_flag = true,
    disable_help_subcommand = true,
    disable_version_flag = true
)]
pub struct Cli {
    /// Indent the JSON result for humans (the same document, never a different one).
    #[arg(long, global = true)]
    pub pretty: bool,
    /// Explicit machine-readable JSON output (also the default).
    #[arg(long, global = true)]
    pub json: bool,
    /// Execute locally by default; submit the same input as an authenticated cloud job.
    #[arg(long, global = true)]
    pub cloud: bool,

    /// Print the CLI version as JSON.
    #[arg(long)]
    pub version: bool,
    /// With --cloud: the workspace you expect the login to act in; a different one is refused (workspace_mismatch).
    #[arg(
        id = "cloud_workspace",
        long = "workspace",
        global = true,
        env = "SIMFORGE_WORKSPACE_ID"
    )]
    pub workspace: Option<String>,
    /// With --cloud: a key that makes resending a lost submission safe (the host answers with the original run and charges once).
    #[arg(long, global = true)]
    pub idempotency_key: Option<String>,
    /// With --cloud: price the job (costEstimateCents) and start nothing.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Hosted API base URL for API-key use. Default: SIMFORGE_API_URL, then the signed-in host's /api/v1.
    #[arg(long, global = true)]
    pub api_url: Option<String>,
    /// Exact hosted map version for a scenario file. Default: the installed release's, else the one hosted version carrying the file's OpenDRIVE digest.
    #[arg(long, global = true)]
    pub map_version: Option<String>,

    #[command(subcommand)]
    pub command: Option<commands::Command>,
}

/// The full clap tree, built (global flags propagated).
pub fn command_tree() -> clap::Command {
    let mut cmd = Cli::command();
    cmd.build();
    cmd
}

fn wants_help(argv: &[String]) -> bool {
    argv.iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--help" || a == "-h")
        || argv.first().map(String::as_str) == Some("help")
}

pub fn help_surface() -> serde_json::Value {
    let root = command_tree();
    let mut doc = help::surface(&root, &root, &[]);
    let legacy: serde_json::Value =
        serde_json::from_str(include_str!("../runtime-help.json")).expect("generated runtime help");
    let globals = doc["globalFlags"].as_array().cloned().unwrap_or_default();
    if let Some(commands) = doc["commands"].as_array_mut() {
        commands.retain(|c| {
            !commands::is_runtime_command(
                c["command"]
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or(""),
            )
        });
        for mut command in legacy["commands"].as_array().into_iter().flatten().cloned() {
            if commands::is_runtime_command(
                command["command"]
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or(""),
            ) {
                if let Some(flags) = command["flags"].as_array_mut() {
                    flags.retain(|f| f["global"] != true && f["name"] != "--json");
                    flags.extend(globals.iter().cloned());
                }
                command["execution"] = json!({"default":"local-runtime","cloud":command["command"].as_str().is_some_and(|s| s.starts_with("scenario "))});
                commands.push(command);
            }
        }
        for command in commands.iter_mut() {
            let name = command["command"].as_str().unwrap_or("").to_owned();
            if !matches!(
                name.as_str(),
                "render" | "simulate" | "preview frame" | "preview clip"
            ) {
                continue;
            }
            if let Some(old) = legacy["commands"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|item| item["command"] == name)
            {
                let existing: std::collections::HashSet<String> = command["flags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|flag| flag["name"].as_str().map(str::to_owned))
                    .collect();
                let extra: Vec<_> = old["flags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|flag| {
                        !existing.contains(flag["name"].as_str().unwrap_or(""))
                            && flag["name"] != "--engine"
                            && flag["name"] != "--json"
                    })
                    .cloned()
                    .map(|mut flag| {
                        flag["execution"] = json!("local-runtime");
                        flag
                    })
                    .collect();
                if let Some(flags) = command["flags"].as_array_mut() {
                    flags.extend(extra);
                }
            }
        }
        add_cloud_flags(commands);
        commands.push(commands::scenario_pull::help(&root));
    }
    doc
}

/// The local-runtime commands that also run with `--cloud` (`transfer`, `import log`, `evaluate`):
/// their cloud-only flags join the runtime's, marked `"execution": "cloud"`. A flag only the cloud
/// requires is optional for the entry as a whole and says so; a positional the cloud reads
/// differently documents both meanings.
fn add_cloud_flags(commands: &mut [serde_json::Value]) {
    use clap::{Args, CommandFactory};
    let cloud = [
        (
            "transfer",
            commands::unified_cloud::transfer::TransferCloudArgs::command(),
        ),
        (
            "import log",
            commands::cloud_log::ImportLogCloudArgs::command(),
        ),
        (
            "evaluate",
            commands::cloud::EvaluateCloudArgs::augment_args(clap::Command::new("evaluate")),
        ),
    ];
    for (name, cmd) in cloud {
        let Some(entry) = commands.iter_mut().find(|c| c["command"] == name) else {
            continue;
        };
        entry["execution"]["cloud"] = json!(true);
        let index = name.split_whitespace().count();
        let positionals = cmd.get_positionals().map(|arg| {
            (
                arg.get_value_names()
                    .map(|n| n.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")),
                arg.get_help().map(|h| h.to_string()).unwrap_or_default(),
            )
        });
        if let Some(arguments) = entry["arguments"].as_array_mut() {
            for (argument, (value_name, help)) in arguments.iter_mut().zip(positionals) {
                if value_name.is_some_and(|v| argument["valueName"] != v.as_str()) {
                    let local = argument["help"].as_str().unwrap_or_default();
                    argument["help"] = json!(format!(
                        "{}. With --cloud: {help}",
                        local.trim_end_matches('.')
                    ));
                }
            }
        }
        let Some(flags) = entry["flags"].as_array_mut() else {
            continue;
        };
        for mut flag in help::flags_of(&cmd) {
            if flags.iter().any(|f| f["name"] == flag["name"]) {
                continue;
            }
            if flag["required"] == true {
                flag["required"] = json!(false);
                let text = flag["help"].as_str().unwrap_or_default();
                flag["help"] = json!(format!("{text} Required with --cloud."));
            }
            flag["execution"] = json!("cloud");
            flag["commandIndex"] = json!(index);
            flag["positional"] = json!(false);
            flags.push(flag);
        }
    }
}

fn help(argv: &[String]) -> Result<i32, CliError> {
    let root = command_tree();
    let mut doc = help_surface();
    let (path, unknown) = help::locate_surface(&root, &doc, argv);
    if let Some(word) = unknown {
        return Err(CliError::new(
            "unknown_command",
            format!("unknown command `{word}`"),
        ));
    }
    if !path.is_empty() {
        let prefix = path.join(" ");
        let selected: Vec<_> = doc["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| {
                let name = c["command"].as_str().unwrap_or("");
                name == prefix || name.starts_with(&format!("{prefix} "))
            })
            .cloned()
            .collect();
        doc["command"] = json!(prefix);
        doc["commands"] = json!(selected);
    }
    emit(&doc, argv.iter().any(|a| a == "--pretty"));
    Ok(Exit::Ok.code())
}

fn context_string(error: &clap::Error, kind: ContextKind) -> Option<String> {
    match error.get(kind)? {
        ContextValue::String(s) => Some(s.clone()),
        ContextValue::Strings(v) => Some(v.join(", ")),
        other => Some(other.to_string()),
    }
}

/// clap's parse failures in the contract's shape. Unknown flags are errors,
/// never warnings: a typo'd flag that is ignored is exactly the failure an
/// unattended caller cannot see. `rest` is argv without the binary name.
pub fn parse_error(error: &clap::Error, rest: &[String]) -> CliError {
    let code = match error.kind() {
        ErrorKind::UnknownArgument => "unknown_flag",
        ErrorKind::InvalidSubcommand => "unknown_command",
        ErrorKind::MissingRequiredArgument => "missing_argument",
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            "missing_command"
        }
        ErrorKind::InvalidValue
        | ErrorKind::ValueValidation
        | ErrorKind::InvalidUtf8
        | ErrorKind::NoEquals => "bad_value",
        ErrorKind::ArgumentConflict => "conflicting_arguments",
        ErrorKind::TooManyValues | ErrorKind::TooFewValues | ErrorKind::WrongNumberOfValues => {
            "bad_value"
        }
        _ => "bad_arguments",
    };
    // `--preset <PRESET>` -> `--preset`; `<NAME@VERSION>` stays as is.
    let path = context_string(error, ContextKind::InvalidArg)
        .or_else(|| context_string(error, ContextKind::InvalidSubcommand))
        .map(|p| {
            if p.starts_with("--") {
                p.split_whitespace().next().unwrap_or(&p).to_owned()
            } else {
                p
            }
        });
    let rendered = error.render().to_string();
    let first_line = rendered
        .lines()
        .next()
        .unwrap_or("invalid arguments")
        .trim_start_matches("error: ")
        .trim()
        .to_owned();
    let mut detail = serde_json::Map::new();
    let reason = match error.kind() {
        ErrorKind::MissingRequiredArgument => format!(
            "missing required argument {}",
            path.as_deref().unwrap_or("")
        ),
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let root = command_tree();
            let (cmd, at, _) = help::locate(&root, rest);
            let known: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
            detail.insert("known".into(), json!(known));
            format!("`simforge {}` needs a subcommand", at.join(" "))
        }
        _ => first_line,
    };
    let mut out = CliError::new(code, reason);
    if let Some(path) = path {
        out = out.with_path(path);
    }
    if let Some(valid) = context_string(error, ContextKind::ValidValue)
        .or_else(|| context_string(error, ContextKind::ValidSubcommand))
    {
        detail.insert("valid".into(), json!(valid));
    }
    if let Some(suggested) = context_string(error, ContextKind::SuggestedArg)
        .or_else(|| context_string(error, ContextKind::SuggestedSubcommand))
    {
        detail.insert("didYouMean".into(), json!(suggested));
    }
    if !detail.is_empty() {
        out = out.with_detail(serde_json::Value::Object(detail));
    }
    out
}

fn normalized_argv(argv: &[OsString]) -> Vec<OsString> {
    let mut globals = vec![argv.first().cloned().unwrap_or_else(|| "simforge".into())];
    let mut command = Vec::new();
    let mut index = 1;
    while index < argv.len() {
        let word = argv[index].to_string_lossy();
        let flag = word.split('=').next().unwrap_or("");
        let takes_value = matches!(
            flag,
            "--workspace" | "--api-url" | "--idempotency-key" | "--map-version"
        );
        if takes_value
            || matches!(
                flag,
                "--json" | "--cloud" | "--pretty" | "--dry-run" | "--version"
            )
        {
            globals.push(argv[index].clone());
            if takes_value && !word.contains('=') && index + 1 < argv.len() {
                index += 1;
                globals.push(argv[index].clone());
            }
        } else {
            command.push(argv[index].clone());
        }
        index += 1;
    }
    let prefix = match command.first().and_then(|word| word.to_str()) {
        Some("render" | "simulate") => 1,
        Some("preview")
            if command
                .get(1)
                .is_some_and(|word| word == "frame" || word == "clip") =>
        {
            2
        }
        _ => 0,
    };
    if prefix > 0 {
        let mut options = Vec::new();
        let mut remaining = Vec::new();
        let mut index = prefix;
        while index < command.len() {
            let word = command[index].to_string_lossy();
            let flag = word.split('=').next().unwrap_or("");
            let value = matches!(
                flag,
                "--out" | "-o" | "--mode" | "--rig" | "--preset" | "--max-cost" | "--revision"
            ) || (prefix == 2 && matches!(flag, "--camera" | "--at" | "--seconds"));
            if value
                || matches!(flag, "--wait" | "--download")
                || (prefix == 2 && flag == "--force")
            {
                options.push(command[index].clone());
                if value && !word.contains('=') && index + 1 < command.len() {
                    index += 1;
                    options.push(command[index].clone());
                }
            } else {
                remaining.push(command[index].clone());
            }
            index += 1;
        }
        globals.extend(command[..prefix].iter().cloned());
        globals.extend(options);
        globals.extend(remaining);
    } else {
        globals.extend(command);
    }
    globals
}

/// Run the CLI over `argv` (including the binary name); returns the exit code.
pub fn run<I, T>(argv: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    // Before anything opens files: soft RLIMIT_NOFILE -> hard (limits.rs).
    limits::raise_open_files();
    let argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
    let rest: Vec<String> = argv
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    usage::start(&rest);
    let result = execute(&argv, &rest);
    let code = match &result {
        Ok(code) => *code,
        Err(error) => {
            emit_error(error);
            error.exit.code()
        }
    };
    usage::finish(code, result.as_ref().err().map(|e| e.code.as_str()));
    code
}

/// One invocation: its exit code, or the error the contract prints on stderr.
fn execute(argv: &[OsString], rest: &[String]) -> Result<i32, CliError> {
    if rest.is_empty() || wants_help(rest) {
        return help(rest);
    }

    let parsed_argv = normalized_argv(argv);
    let cli = Cli::try_parse_from(&parsed_argv).map_err(|error| parse_error(&error, rest))?;
    let ctx = Ctx {
        pretty: cli.pretty,
        cloud: cli.cloud,
        argv: commands::execution::operation_argv(rest),
        workspace: cli.workspace,
        idempotency_key: cli.idempotency_key,
        dry_run: cli.dry_run,
        api_url: cli.api_url,
        map_version: cli.map_version,
    };
    if ctx.map_version.is_some() && !ctx.cloud {
        return Err(CliError::new(
            "cloud_flag_requires_cloud",
            "--map-version requires --cloud",
        ));
    }

    if cli.version {
        emit(
            &json!({
                "bin": "simforge",
                "version": commands::engine_info::package_version(),
                "engineSemVer": commands::engine_info::engine_sem_ver(),
                "commit": commands::engine_info::COMMIT,
            }),
            ctx.pretty,
        );
        return Ok(Exit::Ok.code());
    }
    let Some(command) = cli.command else {
        return help(&[]);
    };

    let outcome = commands::dispatch(command, &ctx)?;
    if !outcome.value.is_null() {
        emit(&outcome.value, ctx.pretty);
    }
    Ok(outcome.exit.code())
}
