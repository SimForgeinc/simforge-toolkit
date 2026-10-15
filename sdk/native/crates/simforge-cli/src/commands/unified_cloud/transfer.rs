//! `simforge transfer SOURCE --map NAME@VERSION --cloud`: the same transfer the local runtime
//! runs, as a hosted `transfer` run. SOURCE is a scenario file (imported and frozen as its own
//! revision, exactly as `render --cloud` treats its target) or a hosted scenario id with
//! `--revision`. A transfer that lands creates a new scenario (a cross-map variation) in the
//! workspace; one no site accepts creates nothing, and its verdict or refusal code is reported
//! as findings, never as success.

use std::path::PathBuf;

use clap::Parser;
use serde_json::{json, Value};

use super::{download_into, report, submit, target};
use crate::commands::cloud::{self, seg, Client};
use crate::contract::{CliError, CmdResult, Ctx, Outcome};

/// The artifacts `--out` receives.
const OUT_KINDS: &[&str] = &["scenario_document", "transfer_report", "transfer_receipt"];

#[derive(Debug, Parser)]
pub struct TransferCloudArgs {
    /// The source: a scenario JSON file, or a hosted scenario id with --revision.
    #[arg(value_name = "SOURCE")]
    pub source: PathBuf,
    /// The target map: a map version id, a name (its newest version) or `name@vN`.
    #[arg(long, value_name = "NAME@VERSION")]
    pub map: String,
    /// The immutable revision of a hosted scenario SOURCE.
    #[arg(long, value_name = "REVISION_ID")]
    pub revision: Option<String>,
    /// The new scenario's name. Default: `<source title> · <target map>`.
    #[arg(long)]
    pub name: Option<String>,
    /// How target sites are searched.
    #[arg(long, value_parser = ["network", "corridor"])]
    pub sites: Option<String>,
    /// Which actors transfer.
    #[arg(long, value_parser = ["pure", "generalized", "key-only"])]
    pub profile: Option<String>,
    /// Whether a crosswalk crossing must land at a target crosswalk.
    #[arg(long, value_parser = ["any", "faithful"])]
    pub crossing: Option<String>,
    /// Transfer the source's original log, not its edited content.
    #[arg(long)]
    pub from_root: bool,
    /// A registered transfer policy version. Default: the current one.
    #[arg(long, value_name = "VERSION")]
    pub policy_version: Option<String>,
    /// Explicit maximum cloud cost in cents.
    #[arg(long, value_name = "CENTS")]
    pub max_cost: Option<u64>,
    /// Wait for the transfer to finish.
    #[arg(long)]
    pub wait: bool,
    /// Wait and download the new scenario, the transfer report and its receipt here.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out: Option<PathBuf>,
}

/// The run input beside the source revision (which `submit` fills in).
fn transfer_input(a: &TransferCloudArgs, target_map_version_id: &str) -> Value {
    let mut input = json!({ "targetMapVersionId": target_map_version_id });
    for (key, value) in [
        ("name", &a.name),
        ("sites", &a.sites),
        ("profile", &a.profile),
        ("crossing", &a.crossing),
        ("policyVersion", &a.policy_version),
    ] {
        if let Some(v) = value {
            input[key] = json!(v);
        }
    }
    if a.from_root {
        input["fromRoot"] = json!(true);
    }
    input
}

/// The target map version the workspace can read, by the reference `--map` names.
fn target_map(c: &Client, reference: &str) -> Result<Value, CliError> {
    let map = c
        .get(&format!("/maps/{}", seg(reference)))
        .map_err(|e| e.with_path("--map"))?;
    if map.get("id").and_then(Value::as_str).is_none() {
        return Err(CliError::new("invalid_api_response", "map id missing"));
    }
    Ok(map)
}

/// Whether a finished transfer created a scenario; a refusal or rejected verdict did not.
fn landed(run: &Value) -> bool {
    run["status"] == "succeeded"
        && run
            .pointer("/result/scenarioId")
            .is_some_and(Value::is_string)
}

/// `argv` is what follows `transfer`.
pub fn cloud_transfer(argv: &[String], ctx: &Ctx) -> CmdResult {
    let a = TransferCloudArgs::try_parse_from(
        std::iter::once("transfer".to_owned()).chain(argv.iter().cloned()),
    )
    .map_err(|error| crate::parse_error(&error, &ctx.argv))?;
    let source = target(&a.source, a.revision.as_deref())?;
    let c = cloud::client(ctx)?;
    let map = target_map(&c, &a.map)?;
    let input = transfer_input(&a, map["id"].as_str().unwrap_or_default());
    let wait = a.wait || a.out.is_some();
    let (run, rev) = match submit(&c, ctx, source, "transfer", input, a.max_cost, wait)? {
        Ok(done) => done,
        Err(dry) => return Ok(Outcome::ok(dry)),
    };
    let mut extra = json!({
        "targetMap": { "id": map["id"], "name": map["name"], "version": map["version"] },
        "transfer": run["result"],
    });
    if let Some(out) = a.out.as_deref().filter(|_| run["status"] == "succeeded") {
        let run_id = run["id"].as_str().unwrap_or_default();
        let planned = cloud::run_artifacts(&c, run_id)?
            .into_iter()
            .filter(|art| OUT_KINDS.contains(&art["kind"].as_str().unwrap_or("")))
            .map(|art| (art["name"].as_str().unwrap_or_default().to_owned(), art))
            .collect();
        extra["out"] = json!(out);
        extra["artifacts"] = json!(download_into(&c, out, planned)?);
    }
    let outcome = report(run, &rev, extra);
    // Waited and finished, but nothing landed: the verdict or refusal is the finding.
    if wait && outcome.value["run"]["status"] == "succeeded" && !landed(&outcome.value["run"]) {
        return Ok(Outcome::findings(outcome.value));
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> TransferCloudArgs {
        TransferCloudArgs::try_parse_from(
            ["transfer", "scenario.json", "--map", "yale-street@v9"]
                .iter()
                .chain(extra),
        )
        .unwrap()
    }

    #[test]
    fn only_the_given_options_are_sent() {
        assert_eq!(
            transfer_input(&args(&[]), "usmap_1"),
            json!({"targetMapVersionId":"usmap_1"})
        );
        let a = args(&[
            "--sites",
            "corridor",
            "--profile",
            "key-only",
            "--crossing",
            "faithful",
            "--from-root",
            "--name",
            "left turn on Yale",
            "--max-cost",
            "10",
        ]);
        assert_eq!(
            transfer_input(&a, "usmap_1"),
            json!({"targetMapVersionId":"usmap_1","sites":"corridor","profile":"key-only",
                   "crossing":"faithful","fromRoot":true,"name":"left turn on Yale"})
        );
        assert_eq!(a.max_cost, Some(10));
    }

    #[test]
    fn same_site_transfers_are_not_offered() {
        assert!(TransferCloudArgs::try_parse_from([
            "transfer", "s.json", "--map", "m", "--sites", "same"
        ])
        .is_err());
    }

    #[test]
    fn a_refusal_or_rejection_is_not_a_landed_transfer() {
        assert!(landed(
            &json!({"status":"succeeded","result":{"verdict":"accepted","scenarioId":"doc_1"}})
        ));
        assert!(!landed(
            &json!({"status":"succeeded","result":{"outcome":"refused","code":"no_site","scenarioId":null}})
        ));
        assert!(!landed(&json!({"status":"failed","result":null})));
    }
}
