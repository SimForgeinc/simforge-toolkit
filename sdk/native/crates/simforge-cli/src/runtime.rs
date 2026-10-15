use crate::contract::{CliError, CmdResult, Exit, Outcome};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const SCHEMA: &str = "simforge.runtime/v1";
const MAX_REPLY: u64 = 64 * 1024 * 1024;

pub fn installation_root() -> Result<PathBuf, CliError> {
    if let Some(root) = std::env::var_os("SIMFORGE_RUNTIME_ROOT") {
        return Ok(crate::paths::absolutize(Path::new(&root)));
    }
    Ok(crate::paths::data_home()?.value.join("simforge/runtime"))
}

pub fn executable() -> Result<PathBuf, CliError> {
    if let Some(path) = std::env::var_os("SIMFORGE_RUNTIME_BIN") {
        let path = crate::paths::absolutize(Path::new(&path));
        if !path.is_file() {
            return Err(CliError::new(
                "runtime_not_installed",
                "SIMFORGE_RUNTIME_BIN does not name a file",
            ));
        }
        // An explicitly named binary may be a development build without a release manifest.
        crate::commands::distribution::verify_runtime_binary(&path)?;
        return Ok(path);
    }
    let name = if cfg!(windows) {
        "simforge-runtime.exe"
    } else {
        "simforge-runtime"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join(name);
            if sibling.is_file() {
                return checked(sibling);
            }
        }
    }
    let installed = installation_root()?.join("bin").join(name);
    if installed.is_file() {
        return checked(installed);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let path = dir.join(name);
            if path.is_file() {
                return checked(path);
            }
        }
    }
    Err(CliError::new(
        "runtime_not_installed",
        "the closed SimForge runtime is not installed",
    ))
}

/// A runtime found by location (beside this CLI, the install root or PATH) must be an installed
/// release: its `runtime-manifest.json` binds the binary's digest and turns on usage reporting.
fn checked(path: PathBuf) -> Result<PathBuf, CliError> {
    if !path.with_file_name("runtime-manifest.json").is_file() {
        return Err(CliError::new(
            "runtime_install_invalid",
            format!(
                "{} has no runtime-manifest.json beside it: it is not an installed SimForge runtime release",
                path.display()
            ),
        )
        .with_detail(json!({ "hint": "Reinstall with simforge runtime install --accept-license (staff development builds: set SIMFORGE_RUNTIME_BIN)." })));
    }
    crate::commands::distribution::verify_runtime_binary(&path)?;
    Ok(path)
}

fn runtime_command(exe: &Path) -> Result<Command, CliError> {
    let mut command = Command::new(exe);
    command
        .env("SIMFORGE_CLI_VERSION", env!("CARGO_PKG_VERSION"))
        .env_remove("SIMFORGE_TOKEN")
        .env_remove("SIMFORGE_API_KEY")
        .env_remove("SIMFORGE_MAPS_REGISTRY_TOKEN");
    if let Some(root) = exe.parent().and_then(Path::parent) {
        let sky = root.join("share/sky");
        let actors = root.join("share/actors");
        if sky.is_dir() && std::env::var_os("SIMFORGE_SKY_ASSETS").is_none() {
            command.env("SIMFORGE_SKY_ASSETS", sky);
        }
        if actors.is_dir() && std::env::var_os("SIMFORGE_ACTOR_ASSETS_ROOT").is_none() {
            command.env("SIMFORGE_ACTOR_ASSETS_ROOT", actors);
        }
        let bin = root.join("bin");
        let ffmpeg = bin.join(if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        });
        if ffmpeg.is_file() {
            let mut paths = vec![bin];
            if let Some(old) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&old));
            }
            let path = std::env::join_paths(paths)
                .map_err(|e| CliError::new("bad_value", e.to_string()))?;
            command.env("PATH", path);
        }
        #[cfg(target_os = "linux")]
        if root.join("lib").is_dir() {
            let mut paths = vec![root.join("lib")];
            if let Some(old) = std::env::var_os("LD_LIBRARY_PATH") {
                paths.extend(std::env::split_paths(&old));
            }
            let path = std::env::join_paths(paths)
                .map_err(|e| CliError::new("bad_value", e.to_string()))?;
            command.env("LD_LIBRARY_PATH", path);
        }
    }
    Ok(command)
}

struct Session {
    input: Box<dyn Write>,
    output: BufReader<Box<dyn Read>>,
    child: Option<Child>,
    sequence: u64,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Session {
    fn connect() -> Result<Self, CliError> {
        if let Some(socket) = std::env::var_os("SIMFORGE_RUNTIME_SOCKET") {
            #[cfg(unix)]
            {
                let stream = std::os::unix::net::UnixStream::connect(&socket)
                    .map_err(|e| CliError::new("runtime_unreachable", e.to_string()))?;
                let reader = stream
                    .try_clone()
                    .map_err(|e| CliError::new("runtime_unreachable", e.to_string()))?;
                return Ok(Self {
                    input: Box::new(stream),
                    output: BufReader::new(Box::new(reader)),
                    child: None,
                    sequence: 0,
                });
            }
            #[cfg(not(unix))]
            {
                let stream = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&socket)
                    .map_err(|e| CliError::new("runtime_unreachable", e.to_string()))?;
                let reader = stream
                    .try_clone()
                    .map_err(|e| CliError::new("runtime_unreachable", e.to_string()))?;
                return Ok(Self {
                    input: Box::new(stream),
                    output: BufReader::new(Box::new(reader)),
                    child: None,
                    sequence: 0,
                });
            }
        }
        let exe = executable()?;
        let mut command = runtime_command(&exe)?;
        command
            .args(["serve", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().map_err(|e| {
            CliError::new("runtime_launch_failed", format!("{}: {e}", exe.display()))
        })?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| CliError::new("runtime_launch_failed", "runtime stdin unavailable"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| CliError::new("runtime_launch_failed", "runtime stdout unavailable"))?;
        Ok(Self {
            input: Box::new(input),
            output: BufReader::new(Box::new(output)),
            child: Some(child),
            sequence: 0,
        })
    }

    fn request(&mut self, op: &str, params: Value) -> Result<Value, CliError> {
        self.sequence += 1;
        let id = format!("cli-{}-{}", std::process::id(), self.sequence);
        let request = json!({"schema": SCHEMA, "id": id, "op": op, "params": params});
        writeln!(self.input, "{request}")
            .and_then(|_| self.input.flush())
            .map_err(|e| CliError::new("runtime_unreachable", e.to_string()))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut self.output)
            .take(MAX_REPLY + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|e| CliError::new("runtime_unreachable", e.to_string()))?;
        if bytes.is_empty() {
            return Err(CliError::new(
                "runtime_disconnected",
                "runtime closed before returning a response",
            ));
        }
        if bytes.len() as u64 > MAX_REPLY {
            return Err(CliError::new(
                "runtime_invalid_response",
                "runtime response exceeds 64 MiB",
            ));
        }
        let reply: Value = serde_json::from_slice(&bytes)
            .map_err(|e| CliError::new("runtime_invalid_response", e.to_string()))?;
        if reply["schema"] != SCHEMA || reply["id"] != id || reply["op"] != op {
            return Err(CliError::new(
                "runtime_protocol_mismatch",
                "runtime response schema, id or operation differs from the request",
            ));
        }
        match reply["ok"].as_bool() {
            Some(true) if reply.get("result").is_some() => Ok(reply["result"].clone()),
            Some(false) => {
                let e = &reply["error"];
                let code = e["code"].as_str().ok_or_else(|| {
                    CliError::new("runtime_invalid_response", "runtime error code missing")
                })?;
                let reason = e["reason"].as_str().ok_or_else(|| {
                    CliError::new("runtime_invalid_response", "runtime error reason missing")
                })?;
                let mut error = match e["exitCode"].as_i64() {
                    Some(1) => CliError::new(code, reason),
                    Some(2) => CliError::findings(code, reason),
                    _ => {
                        return Err(CliError::new(
                            "runtime_invalid_response",
                            "runtime error exitCode must be 1 or 2",
                        ))
                    }
                };
                if let Some(path) = e["path"].as_str() {
                    error = error.with_path(path);
                }
                if let Some(detail) = e.get("detail") {
                    error = error.with_detail(detail.clone());
                }
                Err(error)
            }
            _ => Err(CliError::new(
                "runtime_invalid_response",
                "runtime response must contain ok and result or error",
            )),
        }
    }
}

pub fn request(op: &str, params: Value) -> Result<Value, CliError> {
    let mut session = Session::connect()?;
    let hello = session.request("handshake", json!({"protocol": 1}))?;
    use sha2::{Digest, Sha256};
    let expected = format!(
        "{:x}",
        Sha256::digest(include_bytes!(
            "../../../../contracts/runtime-protocol.v1.json"
        ))
    );
    if hello["runtimeProtocol"] != 1
        || hello["rendererProtocol"] != 5
        || hello["contractSha256"] != expected
    {
        return Err(CliError::new(
            "protocol_mismatch",
            "runtime protocol or raw contract digest does not match this CLI",
        )
        .with_detail(
            json!({"hint": "Install the runtime version matching this CLI.", "handshake": hello}),
        ));
    }
    if !hello["operations"]
        .as_array()
        .is_some_and(|ops| ops.iter().any(|name| name == op))
    {
        return Err(CliError::new(
            "operation_unavailable",
            format!("runtime does not support {op}"),
        ));
    }
    if op == "handshake" {
        return Ok(hello);
    }
    session.request(op, params)
}

pub fn command(argv: &[String]) -> CmdResult {
    let cwd =
        std::env::current_dir().map_err(|e| CliError::new("cwd_unreadable", e.to_string()))?;
    let result = request("command", json!({"argv": argv, "cwd": cwd}))?;
    outcome(result)
}

pub fn operation(op: &str, argv: &[String], mode: Option<&str>) -> CmdResult {
    let cwd =
        std::env::current_dir().map_err(|e| CliError::new("cwd_unreadable", e.to_string()))?;
    let mut params = json!({"argv": argv, "cwd": cwd});
    if matches!(op, "render" | "preview") {
        params["mode"] = json!(mode.unwrap_or("2d"));
    }
    outcome(request(op, params)?)
}

fn outcome(result: Value) -> CmdResult {
    let exit = match result["exitCode"].as_i64() {
        Some(0) => Exit::Ok,
        Some(2) => Exit::Findings,
        _ => {
            return Err(CliError::new(
                "runtime_invalid_response",
                "command exitCode must be 0 or 2",
            ))
        }
    };
    let value = result
        .get("value")
        .cloned()
        .ok_or_else(|| CliError::new("runtime_invalid_response", "command value missing"))?;
    Ok(Outcome { value, exit })
}

pub fn serve_env(argv: &[String]) -> CmdResult {
    request("handshake", json!({"protocol":1}))?;
    let mut command = runtime_command(&executable()?)?;
    command.arg("command").args(argv);
    serve(command, "runtime episode server")
}

pub fn serve_world(argv: &[String]) -> CmdResult {
    request("handshake", json!({"protocol":1}))?;
    let mut command = runtime_command(&executable()?)?;
    command.args(argv);
    serve(command, "world server")
}

fn serve(mut command: Command, _name: &str) -> CmdResult {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        crate::usage::hand_off();
        let error = command.exec();
        Err(CliError::new("runtime_launch_failed", error.to_string()))
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .map_err(|error| CliError::new("runtime_launch_failed", error.to_string()))?;
        match status.code() {
            Some(0) => Ok(Outcome::already_emitted(Exit::Ok)),
            Some(1) => Ok(Outcome::already_emitted(Exit::CommandError)),
            Some(2) => Ok(Outcome::already_emitted(Exit::Findings)),
            _ => Err(CliError::new(
                "runtime_disconnected",
                format!("{_name} terminated unexpectedly"),
            )),
        }
    }
}

/// `simforge telemetry show`: the installed runtime's pending usage batches,
/// exactly as its sender will post them.
pub fn telemetry(argv: &[String]) -> CmdResult {
    let output = runtime_command(&executable()?)?
        .arg("telemetry")
        .args(argv)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| CliError::new("runtime_launch_failed", e.to_string()))?;
    if output.status.success() {
        let value = serde_json::from_slice(&output.stdout)
            .map_err(|e| CliError::new("runtime_invalid_response", e.to_string()))?;
        return Ok(Outcome::ok(value));
    }
    let error: Value = serde_json::from_slice(&output.stderr)
        .map_err(|e| CliError::new("runtime_invalid_response", e.to_string()))?;
    Err(CliError::new(
        error["code"].as_str().unwrap_or("runtime_invalid_response"),
        error["reason"]
            .as_str()
            .unwrap_or("the runtime reported no reason"),
    ))
}

/// After a contact with `host` (login, token refresh, maps pull, runtime
/// install), start the installed runtime's usage sender in the background.
/// The sender is the runtime's; this only wakes it and never waits for it.
pub fn report_usage(host: &crate::auth::Host) {
    let Ok(exe) = executable() else { return };
    let Ok(mut command) = runtime_command(&exe) else {
        return;
    };
    command
        .args(["telemetry", "flush"])
        .env(crate::auth::host::HOST_ENV, &host.origin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || child.wait());
    }
}
