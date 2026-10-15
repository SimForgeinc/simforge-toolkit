//! `simforge challenge`: the SimForge driving challenge (docs/challenge.md).
//!
//! `status`, `submit` and `submissions` talk to the challenge API with this
//! machine's login (scope `challenge:write`). `submit` pushes the image to the
//! team's repository with a one-hour registry login the API issues, then
//! submits it by digest for the next nightly batch. `validate` and `eval`
//! run the open harness (`simforge-challenge`, `pip install simforge-challenge`):
//! the same sandbox and checks the nightly batch runs, and the final-track simulator on scenes
//! you downloaded.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};

use super::cloud::{self, Client};
use crate::contract::{event, CliError, CmdResult, Ctx, Outcome};

const HARNESS: &str = "simforge-challenge";
const SCOPE: &str = "challenge:write";

#[derive(Debug, Subcommand)]
pub enum ChallengeCommand {
    /// Your team, the submissions you have left and your registry repository.
    Status,
    /// Push a driver image to your team's repository and submit it for the next nightly batch.
    Submit(SubmitArgs),
    /// Your team's submissions, their status and scores.
    Submissions,
    /// Validate a driver image against the submission contract, in the nightly batch's sandbox.
    Validate(HarnessArgs),
    /// Evaluate a driver image in the final-track simulator on scenes you downloaded (`--local`).
    Eval(HarnessArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Track {
    /// SimForge closed loop on hidden scenarios, scored nightly.
    Dev,
    /// The final-track simulator on hidden scenes, a few per month.
    Final,
}

impl Track {
    fn as_str(self) -> &'static str {
        match self {
            Track::Dev => "dev",
            Track::Final => "final",
        }
    }
}

#[derive(Debug, Args)]
pub struct SubmitArgs {
    /// A local docker image (name:tag or id) implementing the challenge driver interface.
    pub image: String,
    #[arg(long, value_enum, default_value = "dev")]
    pub track: Track,
}

#[derive(Debug, Args)]
pub struct HarnessArgs {
    /// Arguments for `simforge-challenge` (see `simforge-challenge --help`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub argv: Vec<String>,
}

pub fn run(command: ChallengeCommand, ctx: &Ctx) -> CmdResult {
    match command {
        ChallengeCommand::Status => Ok(Outcome::ok(get(&cloud::client(ctx)?, "/team")?)),
        ChallengeCommand::Submissions => {
            Ok(Outcome::ok(get(&cloud::client(ctx)?, "/submissions")?))
        }
        ChallengeCommand::Submit(args) => submit(&cloud::client(ctx)?, &args),
        ChallengeCommand::Validate(args) => harness("validate", &args.argv),
        ChallengeCommand::Eval(args) => harness("eval", &args.argv),
    }
}

fn url(client: &Client, path: &str) -> String {
    format!("{}/api/cli/challenge{path}", client.origin())
}

fn scope_hint(error: CliError) -> CliError {
    if error.code == "insufficient_scope" || error.code == "forbidden" {
        let reason = format!(
            "{}; log in with the challenge scope: simforge login --scope {SCOPE}",
            error.reason
        );
        return CliError { reason, ..error };
    }
    error
}

fn get(client: &Client, path: &str) -> Result<Value, CliError> {
    client.get(&url(client, path)).map_err(scope_hint)
}

fn post(client: &Client, path: &str, body: &Value) -> Result<Value, CliError> {
    client.post(&url(client, path), body).map_err(scope_hint)
}

fn text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, CliError> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            CliError::new(
                "invalid_api_response",
                format!("the challenge API returned no {pointer}"),
            )
        })
}

/// Run docker with `config` as its config directory; stdout is captured (the result goes to our stdout alone).
fn docker(config: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, CliError> {
    let mut child = Command::new("docker")
        .env("DOCKER_CONFIG", config)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CliError::new("docker_unavailable", format!("cannot run docker: {e}")))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input.as_bytes())
            .map_err(|e| CliError::new("docker_failed", format!("docker {}: {e}", args[0])))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| CliError::new("docker_failed", format!("docker {}: {e}", args[0])))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .trim()
            .chars()
            .rev()
            .take(600)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(CliError::new(
            "docker_failed",
            format!("docker {} failed: {tail}", args[0]),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A private docker config directory for one push: the registry login never lands in the user's own config.
struct DockerConfig(std::path::PathBuf);

impl DockerConfig {
    fn create(stamp: u64) -> Result<Self, CliError> {
        let dir = std::env::temp_dir().join(format!(
            "simforge-challenge-docker-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).map_err(|e| {
            CliError::new("io_error", format!("cannot create {}: {e}", dir.display()))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(
                |e| {
                    CliError::new(
                        "io_error",
                        format!("cannot restrict {}: {e}", dir.display()),
                    )
                },
            )?;
        }
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for DockerConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The `sha256:<64 hex>` digest a `docker push` reports for the pushed tag.
pub fn pushed_digest(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|word| {
        let hex = word.strip_prefix("sha256:")?;
        (hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        .then(|| format!("sha256:{hex}"))
    })
}

fn submit(client: &Client, args: &SubmitArgs) -> CmdResult {
    let team = get(client, "/team")?;
    let credentials = post(client, "/registry-credentials", &json!({}))?;
    let registry = text(&credentials, "/registry")?;
    let repository = text(&credentials, "/repository")?;
    let username = text(&credentials, "/username")?;
    let password = text(&credentials, "/password")?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let config = DockerConfig::create(stamp)?;
    let target = format!("{registry}/{repository}:{}-{stamp}", args.track.as_str());
    event(json!({ "event": "challenge.submit.push", "image": args.image, "target": target }));
    docker(
        config.path(),
        &[
            "login",
            "--username",
            username,
            "--password-stdin",
            registry,
        ],
        Some(password),
    )?;
    docker(config.path(), &["tag", &args.image, &target], None)?;
    let pushed = docker(config.path(), &["push", &target], None);
    // The tag and the registry login are ours alone; drop both whatever the push did.
    let _ = docker(config.path(), &["image", "rm", &target], None);
    let _ = docker(config.path(), &["logout", registry], None);
    let digest = pushed_digest(&pushed?)
        .ok_or_else(|| CliError::new("docker_failed", "docker push reported no image digest"))?;
    let reference = format!("{registry}/{repository}@{digest}");
    event(json!({ "event": "challenge.submit.pushed", "image": reference }));
    let submission = post(
        client,
        "/submissions",
        &json!({ "track": args.track.as_str(), "image": reference }),
    )?;
    Ok(Outcome::ok(
        json!({ "team": team.get("team").cloned().unwrap_or(Value::Null), "image": reference, "submission": submission }),
    ))
}

/// Forward to the open harness; its JSON report becomes our result, its exit code our verdict.
fn harness(subcommand: &str, argv: &[String]) -> CmdResult {
    let output = Command::new(HARNESS)
        .arg(subcommand)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| {
            CliError::new(
                "harness_unavailable",
                format!("cannot run {HARNESS} ({e}); install the challenge harness: pip install simforge-challenge"),
            )
        })?;
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        CliError::new(
            "harness_failed",
            format!(
                "{HARNESS} {subcommand} wrote no JSON report (exit {:?})",
                output.status.code()
            ),
        )
    })?;
    match output.status.code() {
        Some(0) => Ok(Outcome::ok(report)),
        Some(1) => Ok(Outcome::findings(report)),
        code => Err(CliError::new(
            "harness_failed",
            format!("{HARNESS} {subcommand} exited {code:?}"),
        )
        .with_detail(report)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_read_from_push_output() {
        let output = "dev-1: digest: sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef size: 1573\n";
        assert_eq!(
            pushed_digest(output).as_deref(),
            Some("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(pushed_digest("digest: sha256:abc size: 3"), None);
        assert_eq!(
            pushed_digest(
                "sha256:0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"
            ),
            None
        );
    }
}
