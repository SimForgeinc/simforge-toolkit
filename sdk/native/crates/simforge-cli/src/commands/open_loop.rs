//! Hosted Alpamayo 1.5 evaluation of a succeeded scenario render.
use super::cloud::{self, seg, Client};
use crate::contract::{CliError, CmdResult, Ctx, Outcome};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::{thread, time::Duration};

#[derive(Debug, Subcommand)]
pub enum OpenLoopCommand {
    /// Score a succeeded cloud scenario render with the pinned Alpamayo 1.5 model (requires --cloud, --max-cost and --idempotency-key, also with --dry-run).
    Run(RunArgs),
    /// List the workspace's evaluations.
    List(ListArgs),
    /// One evaluation: status, cost and attempts.
    Show(EvaluationArgs),
    /// Follow an evaluation until it succeeds, is partial, fails or is cancelled.
    Watch(EvaluationArgs),
    /// The evaluation's result manifest: scored metrics, artifacts, model/render/source pins and compute cost.
    Result(EvaluationArgs),
    /// A download grant ({url, sha256, bytes, ...}) for one artifact of the evaluation's result.
    Artifacts(ArtifactArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// The succeeded cloud scenario render run to evaluate.
    #[arg(value_name = "RENDER_RUN_ID")]
    pub render_run_id: String,
    /// Prediction time t0 on the simulation trace clock, in microseconds.
    #[arg(long, value_name = "US", default_value_t = 1_500_000)]
    pub t0_us: u64,
    /// Model sampling seed.
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    /// Trajectory samples drawn by the model.
    #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=64))]
    pub samples: u32,
    /// Explicit maximum cloud cost in cents; required, also with --dry-run.
    #[arg(long, value_name = "CENTS")]
    pub max_cost: u64,
    /// Wait for the evaluation to finish.
    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// At most this many evaluations (1-100).
    #[arg(long)]
    pub limit: Option<u32>,
    /// Only evaluations in these statuses (comma-separated).
    #[arg(long)]
    pub status: Option<String>,
    /// The page after this cursor (`nextCursor` of the previous page).
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Debug, Args)]
pub struct EvaluationArgs {
    /// The evaluation (`cmpj_...`).
    #[arg(value_name = "EVALUATION_ID")]
    pub id: String,
}

#[derive(Debug, Args)]
pub struct ArtifactArgs {
    /// The evaluation (`cmpj_...`).
    #[arg(value_name = "EVALUATION_ID")]
    pub id: String,
    /// The artifact, by its id in the result manifest.
    #[arg(value_name = "ARTIFACT_ID")]
    pub artifact_id: String,
}

const TERMINAL: &[&str] = &["succeeded", "partial", "failed", "cancelled"];

pub fn run(command: OpenLoopCommand, ctx: &Ctx) -> CmdResult {
    match command {
        OpenLoopCommand::Run(args) => submit(&cloud::client(ctx)?, args, ctx),
        OpenLoopCommand::List(args) => list(&cloud::client(ctx)?, &args),
        OpenLoopCommand::Show(args) => Ok(Outcome::ok(cloud::client(ctx)?.get(&path(&args.id))?)),
        OpenLoopCommand::Watch(args) => Ok(cloud::status(wait(&cloud::client(ctx)?, &args.id)?)),
        OpenLoopCommand::Result(args) => Ok(Outcome::ok(
            cloud::client(ctx)?.get(&format!("{}/result", path(&args.id)))?,
        )),
        OpenLoopCommand::Artifacts(args) => Ok(Outcome::ok(cloud::client(ctx)?.post(
            &format!(
                "{}/artifacts/{}/download",
                path(&args.id),
                seg(&args.artifact_id)
            ),
            &json!({}),
        )?)),
    }
}

fn path(id: &str) -> String {
    format!("/evaluations/{}", seg(id))
}

/// The `POST /evaluations` body; the model is pinned by the host, never chosen here.
fn request(args: &RunArgs, dry_run: bool) -> Value {
    json!({
        "renderRunId": args.render_run_id,
        "t0Us": args.t0_us,
        "seed": args.seed,
        "numTrajSamples": args.samples,
        "maxCostCents": args.max_cost,
        "dryRun": dry_run,
    })
}

fn submit(c: &Client, args: RunArgs, ctx: &Ctx) -> CmdResult {
    if !ctx.cloud {
        return Err(CliError::new(
            "missing_argument",
            "open-loop evaluation runs only in the cloud: pass --cloud",
        )
        .with_path("--cloud"));
    }
    if ctx.idempotency_key.is_none() {
        return Err(CliError::new(
            "missing_argument",
            "open-loop evaluation requires --idempotency-key KEY, also with --dry-run",
        )
        .with_path("--idempotency-key"));
    }
    let r = c.post("/evaluations", &request(&args, ctx.dry_run))?;
    if ctx.dry_run {
        return Ok(if r.get("allowed") == Some(&Value::Bool(false)) {
            Outcome::findings(r)
        } else {
            Outcome::ok(r)
        });
    }
    if !args.wait {
        return Ok(Outcome::ok(r));
    }
    let id = r
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("invalid_api_response", "evaluation id missing"))?;
    Ok(cloud::status(wait(c, id)?))
}

fn list(c: &Client, args: &ListArgs) -> CmdResult {
    let mut q = Vec::new();
    if let Some(x) = args.limit {
        q.push(format!("limit={x}"));
    }
    if let Some(x) = &args.status {
        q.push(format!("status={}", seg(x)));
    }
    if let Some(x) = &args.cursor {
        q.push(format!("cursor={}", seg(x)));
    }
    let p = if q.is_empty() {
        "/evaluations".to_owned()
    } else {
        format!("/evaluations?{}", q.join("&"))
    };
    Ok(Outcome::ok(c.get(&p)?))
}

/// Poll `GET /evaluations/:id` until the job reaches a terminal status.
fn wait(c: &Client, id: &str) -> Result<Value, CliError> {
    loop {
        let job = c.get(&path(id))?;
        if job
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| TERMINAL.contains(&s))
        {
            return Ok(job);
        }
        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Input {
        #[command(subcommand)]
        command: OpenLoopCommand,
    }

    #[test]
    fn a_run_without_a_cost_cap_or_with_zero_samples_is_refused() {
        assert!(Input::try_parse_from(["open-loop", "run", "run_1"]).is_err());
        assert!(Input::try_parse_from([
            "open-loop",
            "run",
            "run_1",
            "--max-cost",
            "50",
            "--samples",
            "0"
        ])
        .is_err());
    }
}
