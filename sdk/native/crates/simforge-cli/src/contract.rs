//! The CLI contract (README.md "CLI contract"), in one place.
//!
//! - **stdout is the result**: exactly one JSON document, pretty-printed only
//!   with `--pretty`. Nothing else is ever written there.
//! - **stderr carries the structured error**: one JSON line
//!   `{code, path?, reason, hint, detail?, httpStatus?, requestId?}` (the last
//!   two when a host answered: its HTTP status and request id, for support),
//!   no progress chatter. Event lines
//!   (`{event, ...}`, never a `code`, written by [`event`]) come before it
//!   only where a caller needs them while the command runs: `login`, which
//!   waits on a person, writes the prompt they must act on so an agent can
//!   relay the URL or code; `maps pull --progress` writes
//!   `maps.pull.progress` (blobs and bytes verified out of the release's
//!   total) so a worker can show a long download; `runtime install` and the
//!   signed-in `maps pull` write `distribution.download.progress` (bytes
//!   written out of the archive's, at each tenth and at least every 30 s).
//! - **Exit codes**: `0` ok; `1` the command could not run (bad flags, missing
//!   file, unreachable registry); `2` it ran and found something wrong with
//!   its input (a failed verification, a failed doctor check). Callers key
//!   repair loops off the 1/2 distinction, so it must never blur.

use std::io::Write;

use serde::Serialize;
use serde_json::Value;

/// Write one event (a JSON line) to stderr; see the module docs for who may.
pub fn event(value: Value) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{value}");
    let _ = err.flush();
}

/// Process exit codes. The numeric values are the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[repr(i32)]
pub enum Exit {
    Ok = 0,
    CommandError = 1,
    Findings = 2,
}

impl Exit {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// The exit-code table as the help surface prints it.
pub fn exit_code_table() -> Value {
    serde_json::json!({
        "0": "ok",
        "1": "command error: the command could not run",
        "2": "findings: the command ran and found something wrong with its input",
    })
}

/// A structured failure. `exit` is [`Exit::CommandError`] unless the command
/// ran and is reporting findings about its input.
#[derive(Debug, Clone)]
pub struct CliError {
    pub code: String,
    pub path: Option<String>,
    pub reason: String,
    pub detail: Option<Value>,
    pub exit: Exit,
    /// The HTTP status and request id of the host answer this error reports, if any.
    pub http: Option<(u16, Option<String>)>,
}

impl CliError {
    /// A "could not run" error (exit 1).
    pub fn new(code: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            path: None,
            reason: reason.into(),
            detail: None,
            exit: Exit::CommandError,
            http: None,
        }
    }

    /// A "ran and found something wrong with the input" error (exit 2).
    pub fn findings(code: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            exit: Exit::Findings,
            ..Self::new(code, reason)
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The host answer this error reports: its HTTP status and request id.
    pub fn with_http(mut self, status: u16, request_id: Option<String>) -> Self {
        self.http = Some((status, request_id));
        self
    }

    pub fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("code".into(), Value::String(self.code.clone()));
        if let Some(path) = &self.path {
            map.insert("path".into(), Value::String(path.clone()));
        }
        map.insert("reason".into(), Value::String(self.reason.clone()));
        let hint = self
            .detail
            .as_ref()
            .and_then(|d| d.get("hint"))
            .cloned()
            .unwrap_or_else(|| Value::String(hint(&self.code).to_owned()));
        map.insert("hint".into(), hint);
        if let Some(detail) = &self.detail {
            map.insert("detail".into(), detail.clone());
        }
        if let Some((status, request_id)) = &self.http {
            map.insert("httpStatus".into(), Value::from(*status));
            if let Some(id) = request_id {
                map.insert("requestId".into(), Value::String(id.clone()));
            }
        }
        Value::Object(map)
    }
}

/// What to do about an error, by its code (the host's code for a refused request): the
/// fallback when the error carries no `detail.hint` of its own.
fn hint(code: &str) -> &'static str {
    match code {
        "not_logged_in" | "unauthorized" | "insufficient_scope" => "Run simforge login for the selected host and required scopes.",
        "runtime_not_installed" => "Run simforge runtime install after signing in.",
        "max_cost_required" => "Pass --max-cost CENTS to explicitly cap this cloud job.",
        "budget_exceeded" => "Rerun with --dry-run to see the estimate, then raise --max-cost or add workspace credit.",
        "insufficient_credits" => "Check the workspace's credits (simforge usage); run it without --cloud, or choose free work such as a 2D render.",
        "cloud_not_open" => "Run the command without --cloud to run it locally.",
        "rate_limited" => "Wait for the running job to finish, or retry later; the host's detail names the limit.",
        "file_unreadable" | "invalid_json" => "Check the input path and JSON document.",
        "draft_conflict" => "Read the latest document and explicitly rebase the transaction.",
        "conflict" => "The host found a stale version or digest, or a duplicate: read the object again, then decide.",
        "idempotency_conflict" => "That idempotency key was used for a different request: send this one with a new --idempotency-key.",
        "not_found" => "Check the id, and that it belongs to the selected workspace.",
        "map_pin_required" => "The scenario must pin its map (sourceMap.mapId and sourceMap.xodrSha256): make it with simforge scenario new --map, or instantiate the template again with simforge instantiate --out.",
        "download_failed" => "Check network, DNS and TLS connectivity, then retry the download; verified partial bytes resume.",
        "host_unreachable" => "Check network, DNS and TLS connectivity to the host, then retry.",
        "internal_error" | "api_error" => "Retry; if it fails again, send SimForge support the requestId.",
        _ => "Run simforge --help --json and correct the reported input or prerequisite.",
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.reason)
    }
}

impl std::error::Error for CliError {}

/// What a command produced: the stdout document and the exit code. A command
/// that ran and found problems returns `Exit::Findings` *with* its report on
/// stdout (e.g. `doctor` with a failing check), rather than an error.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub value: Value,
    pub exit: Exit,
}

impl Outcome {
    pub fn ok(value: Value) -> Self {
        Self {
            value,
            exit: Exit::Ok,
        }
    }

    pub fn findings(value: Value) -> Self {
        Self {
            value,
            exit: Exit::Findings,
        }
    }

    /// A long-running command that already wrote its one stdout document
    /// (`env serve` prints its ready line while it serves); nothing more is
    /// emitted when it returns.
    pub fn already_emitted(exit: Exit) -> Self {
        Self {
            value: Value::Null,
            exit,
        }
    }
}

pub type CmdResult = Result<Outcome, CliError>;

/// Flags every command shares.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub pretty: bool,
    pub cloud: bool,
    pub argv: Vec<String>,
    pub workspace: Option<String>,
    pub idempotency_key: Option<String>,
    pub dry_run: bool,
    pub api_url: Option<String>,
    pub map_version: Option<String>,
}

/// Write the result document to stdout.
pub fn emit(value: &Value, pretty: bool) {
    let text = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .expect("a serde_json::Value always serializes");
    let mut out = std::io::stdout().lock();
    // A closed stdout (`simforge ... | head -c0`) is the caller's choice, not an error to report.
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

/// Write a structured error to stderr (always one compact JSON line).
pub fn emit_error(error: &CliError) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{}", error.to_json());
    let _ = err.flush();
}
