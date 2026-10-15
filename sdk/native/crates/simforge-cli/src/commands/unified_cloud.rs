//! `--cloud` for the same-file commands: `render`, `simulate`, `preview`, `transfer`, `scenario new|apply|check`.
//! A scenario file goes to the host exactly: imported as a draft whose canonical digest must be the file's
//! (`POST /drafts` with `content`), on the map version its `sourceMap` pins, frozen as an immutable revision,
//! and only that revision runs. A hosted `SCENARIO_ID --revision REVISION_ID` target runs as given.
//! Artifacts land under `--out` in the layout the local runtime writes.
mod hosted;
mod map_pin;
mod preview_manifest;
pub(crate) mod store;
pub(crate) mod transfer;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};

use super::cloud::{self, Client};
use super::cloud_drafts::{capabilities, draft_path, require_available};
use super::execution::{ExecutionArgs, PreviewArgs, RenderMode};
use crate::authoring_ops::{self, Operations};
use crate::contract::{CliError, CmdResult, Ctx, Exit, Outcome};
use crate::rig::{preview_rig, PreviewSpan};

/// How long a freeze waits for the host's authoritative simulation of the imported draft.
const FREEZE_PATIENCE: Duration = Duration::from_secs(15 * 60);

enum Target {
    File(PathBuf),
    Hosted {
        scenario_id: String,
        revision_id: String,
    },
}

fn target(path: &Path, revision: Option<&str>) -> Result<Target, CliError> {
    if path.is_file() {
        if revision.is_some() {
            return Err(CliError::new(
                "bad_value",
                "--revision names a hosted scenario's revision; a scenario file is frozen as its own revision",
            )
            .with_path("--revision"));
        }
        return Ok(Target::File(path.to_path_buf()));
    }
    if path.exists() {
        return Err(CliError::new(
            "cloud_target_unsupported",
            format!(
                "{} is not a scenario JSON file; a materialized workspace runs locally",
                path.display()
            ),
        )
        .with_path("TARGET"));
    }
    match revision {
        Some(revision_id) => Ok(Target::Hosted {
            scenario_id: path.to_string_lossy().into_owned(),
            revision_id: revision_id.to_owned(),
        }),
        None => Err(CliError::new(
            "file_not_found",
            format!("{} does not exist", path.display()),
        )
        .with_path("TARGET")
        .with_detail(json!({ "hint": "pass a scenario JSON file, or a hosted scenario id with --revision REVISION_ID" }))),
    }
}

fn refuse_extra(extra: &[String]) -> Result<(), CliError> {
    if extra.is_empty() {
        return Ok(());
    }
    Err(CliError::new(
        "cloud_option_unsupported",
        format!(
            "runtime options are not sent to the cloud: {}",
            extra.join(" ")
        ),
    )
    .with_detail(json!({ "options": extra, "hint": "drop them, or run without --cloud" })))
}

fn str_at<'a>(v: &'a Value, pointer: &str) -> Result<&'a str, CliError> {
    v.pointer(pointer).and_then(Value::as_str).ok_or_else(|| {
        CliError::new(
            "invalid_api_response",
            format!("{pointer} missing from the host's answer"),
        )
    })
}

/// The document's map pin; a document without one cannot be imported exactly.
fn source_map(document: &Value, file: &Path) -> Result<(String, String), CliError> {
    let map = &document["sourceMap"];
    match (map["mapId"].as_str(), map["xodrSha256"].as_str()) {
        (Some(id), Some(xodr)) => Ok((id.to_owned(), xodr.to_owned())),
        _ => Err(CliError::findings(
            "map_pin_required",
            format!(
                "{} does not pin its map (sourceMap.mapId and sourceMap.xodrSha256)",
                file.display()
            ),
        )
        .with_path(file.display().to_string())),
    }
}

fn draft_name(document: &Value, file: &Path) -> String {
    let name = document
        .pointer("/meta/name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            file.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "scenario".into())
        });
    name.chars().take(256).collect()
}

/// Import `document` as a draft and check the host holds exactly it, on its own pinned map.
fn import(
    c: &Client,
    file: &Path,
    document: &Value,
    digest: &str,
    provisional: bool,
    explicit_map: Option<&str>,
) -> Result<Value, CliError> {
    let (map_id, xodr) = source_map(document, file)?;
    let pin = map_pin::request(document, explicit_map)?;
    let mut body = json!({"name":draft_name(document,file),"content":document,"contentDigest":digest,"provisional":provisional});
    body.as_object_mut()
        .unwrap()
        .extend(pin.as_object().unwrap().clone());
    let receipt = c.fresh().post("/drafts", &body)?;
    let draft_id = str_at(&receipt, "/documentId")?.to_owned();
    if str_at(&receipt, "/operationDigest")? != digest {
        return Err(CliError::new(
            "content_identity_mismatch",
            "the host's draft is not the file's content",
        )
        .with_detail(
            json!({ "expected": digest, "host": receipt["operationDigest"], "draftId": draft_id }),
        ));
    }
    if str_at(&receipt, "/mapPin/mapId")? != map_id
        || str_at(&receipt, "/mapPin/xodrSha256")? != xodr
    {
        return Err(CliError::new(
            "map_pin_mismatch",
            "the host pinned the draft to another map than the file",
        )
        .with_detail(json!({ "pinned": { "mapId": map_id, "xodrSha256": xodr }, "host": receipt["mapPin"], "draftId": draft_id })));
    }
    if let Some(release) = pin.get("mapReleaseDigest") {
        if receipt["mapPin"]["releaseDigest"] != *release {
            return Err(CliError::new(
                "map_pin_mismatch",
                "hosted draft release differs from the installed map",
            )
            .with_detail(
                json!({"draftId":draft_id,"installed":release,"host":receipt["mapPin"]}),
            ));
        }
    }
    Ok(receipt)
}

/// Freeze the imported draft as an immutable revision, waiting out the host's authoritative simulation.
fn freeze(c: &Client, receipt: &Value) -> Result<Value, CliError> {
    let draft_id = str_at(receipt, "/documentId")?;
    let body = json!({
        "expectedVersion": receipt["draftVersion"],
        "expectedDigest": receipt["contentDigest"],
    });
    // One idempotency key for every attempt: a retried freeze is the same revision.
    let client = c.fresh();
    let started = Instant::now();
    loop {
        match client.post(&draft_path(draft_id, Some("freeze-revision")), &body) {
            Ok(frozen) => {
                if frozen["contentDigest"] != receipt["contentDigest"] {
                    return Err(CliError::new(
                        "content_identity_mismatch",
                        "the frozen revision is not the imported content",
                    )
                    .with_detail(json!({ "expected": receipt["contentDigest"], "frozen": frozen["contentDigest"], "draftId": draft_id })));
                }
                return Ok(frozen);
            }
            Err(e)
                if e.detail.as_ref().and_then(|d| d["code"].as_str())
                    == Some("simulation_pending")
                    && started.elapsed() < FREEZE_PATIENCE =>
            {
                thread::sleep(Duration::from_secs(3));
            }
            Err(mut e) => {
                let mut detail = e.detail.take().unwrap_or_else(|| json!({}));
                if let Some(map) = detail.as_object_mut() {
                    map.insert("draftId".into(), json!(draft_id));
                    map.insert("hint".into(), json!("the draft stays in the workspace; freeze it with `simforge drafts freeze` or delete it"));
                }
                return Err(e.with_detail(detail));
            }
        }
    }
}

/// The immutable revision a run executes, and the ids this command created on the host.
struct Revision {
    scenario_id: String,
    revision_id: String,
    created: Value,
    source: Value,
}

fn revision(c: &Client, target: Target, ctx: &Ctx) -> Result<Revision, CliError> {
    match target {
        Target::Hosted {
            scenario_id,
            revision_id,
        } => Ok(Revision {
            source: json!({ "scenarioId": scenario_id, "revisionId": revision_id }),
            scenario_id,
            revision_id,
            created: json!({}),
        }),
        Target::File(file) => hosted::file_revision(c, &file, ctx),
    }
}

fn require_cap(ctx: &Ctx, max_cost: Option<u64>) -> Result<(), CliError> {
    if ctx.dry_run || max_cost.is_some() {
        return Ok(());
    }
    Err(CliError::new(
        "max_cost_required",
        "cloud execution requires an explicit --max-cost in cents",
    )
    .with_path("--max-cost"))
}

/// Submit `run_type` on the target's revision; returns the dry-run answer, or the run (waited when asked).
fn submit(
    c: &Client,
    ctx: &Ctx,
    target: Target,
    run_type: &str,
    input: Value,
    max_cost: Option<u64>,
    wait: bool,
) -> Result<Result<(Value, Revision), Value>, CliError> {
    require_cap(ctx, max_cost)?;
    let rev = revision(c, target, ctx)?;
    let mut input = input;
    input["scenarioId"] = json!(rev.scenario_id);
    input["revisionId"] = json!(rev.revision_id);
    let created = c.post(
        "/runs",
        &cloud::run_request(run_type, input, ctx.dry_run, max_cost),
    )?;
    if ctx.dry_run {
        return Ok(Err(hosted::priced(created, &rev)));
    }
    let run_id = str_at(&created, "/id")?.to_owned();
    let run = if wait {
        cloud::wait_public(c, &run_id)?
    } else {
        created
    };
    if run["input"]["revisionId"] != json!(rev.revision_id) {
        return Err(CliError::new(
            "invalid_api_response",
            "the run is not on the submitted revision",
        )
        .with_detail(json!({ "runId": run_id, "revisionId": rev.revision_id })));
    }
    let mut rev = rev;
    rev.created["runId"] = json!(run_id);
    Ok(Ok((run, rev)))
}

fn report(run: Value, rev: &Revision, extra: Value) -> Outcome {
    let mut value = json!({ "run": run, "source": rev.source, "created": rev.created });
    for (k, v) in extra.as_object().into_iter().flatten() {
        value[k] = v.clone();
    }
    match value["run"]["status"].as_str() {
        Some("failed" | "partial" | "cancelled") => Outcome::findings(value),
        _ => Outcome::ok(value),
    }
}

/// `out/<path>` for each artifact `(path, artifact)`, refusing escapes and two artifacts on one path.
fn download_into(
    c: &Client,
    out: &Path,
    planned: Vec<(String, Value)>,
) -> Result<Vec<Value>, CliError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut done = Vec::new();
    for (rel, art) in planned {
        let path = cloud::safe(&rel)?;
        if !seen.insert(path.clone()) {
            return Err(CliError::new(
                "artifact_path_collision",
                format!("two artifacts map to {rel}"),
            )
            .with_path("--out"));
        }
        let target = out.join(&path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CliError::new("write_failed", format!("{}: {e}", parent.display()))
                    .with_path("--out")
            })?;
        }
        let mut got = cloud::download_artifact(c, &art, &target)?;
        got["path"] = json!(rel);
        got["artifact"] = art["name"].clone();
        done.push(got);
    }
    Ok(done)
}

/// A render's artifacts at the paths the local render writes under its `--out`.
fn render_layout(artifacts: Vec<Value>) -> Vec<(String, Value)> {
    artifacts
        .into_iter()
        .map(|art| {
            let name = art["name"].as_str().unwrap_or_default();
            let rel = ["render-2d/", "render-3d/"]
                .iter()
                .find_map(|p| name.strip_prefix(p))
                .unwrap_or(name)
                .to_owned();
            (rel, art)
        })
        .collect()
}

fn render_input(
    mode: RenderMode,
    preset: Option<&str>,
    rig: Option<Value>,
) -> Result<Value, CliError> {
    let (engine, preset) = match (mode, preset) {
        (RenderMode::TwoD, Some(_)) => {
            return Err(CliError::new(
                "bad_value",
                "--preset applies to the 3d and schematic modes",
            )
            .with_path("--preset"))
        }
        (RenderMode::TwoD, None) => ("2d", None),
        (RenderMode::Schematic, None | Some("schematic")) => ("3d", Some("schematic")),
        (RenderMode::Schematic, Some(p)) => {
            return Err(CliError::new(
                "bad_value",
                format!("--mode schematic renders the schematic preset, not {p}"),
            )
            .with_path("--preset"))
        }
        (RenderMode::ThreeD, Some("schematic")) => {
            return Err(CliError::new(
                "conflicting_arguments",
                "--preset schematic requires --mode schematic",
            ))
        }
        (RenderMode::ThreeD, p) => ("3d", p),
    };
    let mut input = json!({ "engine": engine });
    if let Some(p) = preset {
        input["preset"] = json!(p);
    }
    if let Some(r) = rig {
        input["rig"] = r;
    }
    Ok(input)
}

/// `render TARGET --cloud`: the target's revision rendered, its artifacts under `--out` as a local render writes them.
pub fn cloud_render(a: &ExecutionArgs, ctx: &Ctx) -> CmdResult {
    refuse_extra(&a.extra)?;
    let rig = a
        .rig
        .as_deref()
        .map(|p| crate::rig::read_file(p, "--rig"))
        .transpose()?;
    let input = render_input(a.mode, a.preset.as_deref(), rig)?;
    let target = target(&a.target, a.revision.as_deref())?;
    let c = cloud::client(ctx)?;
    let wait = a.wait || a.download || a.out.is_some();
    let (run, rev) = match submit(&c, ctx, target, "render", input, a.max_cost, wait)? {
        Ok(done) => done,
        Err(dry) => return Ok(Outcome::ok(dry)),
    };
    let mut extra = json!({ "mode": a.mode.as_str() });
    if let Some(out) = a.out.as_deref().filter(|_| run["status"] == "succeeded") {
        let run_id = str_at(&run, "/id")?;
        let files = download_into(&c, out, render_layout(cloud::run_artifacts(&c, run_id)?))?;
        extra["out"] = json!(out);
        extra["artifacts"] = json!(files);
    }
    Ok(report(run, &rev, extra))
}

/// The run's trace among its artifacts: the one the run reports, else the only one.
fn pick_trace<'a>(run: &Value, traces: &[&'a Value]) -> Result<&'a Value, CliError> {
    if let Some(sha) = run.pointer("/result/traceSha256").and_then(Value::as_str) {
        let named = traces.iter().find(|t| {
            t["sha256"] == sha
                || t["name"]
                    .as_str()
                    .and_then(|n| n.rsplit('/').next())
                    .is_some_and(|b| b.starts_with(sha))
        });
        if let Some(t) = named {
            return Ok(*t);
        }
    }
    match traces {
        [only] => Ok(*only),
        [] => Err(CliError::new(
            "simulate_output_missing",
            "the run returned no trace",
        )),
        _ => Err(CliError::new(
            "simulate_trace_ambiguous",
            "the run returned several traces and names none",
        )
        .with_detail(
            json!({ "traces": traces.iter().map(|t| t["name"].clone()).collect::<Vec<_>>() }),
        )),
    }
}

/// Write the downloaded trace at `from` to `to` as plain JSON (gunzipped when it is gzip), returning its sha256.
fn normalize_trace(from: &Path, to: &Path) -> Result<String, CliError> {
    let bytes = fs::read(from)
        .map_err(|e| CliError::new("io_error", format!("{}: {e}", from.display())))?;
    let plain = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut out)
            .map_err(|e| {
                CliError::new("trace_invalid", format!("the trace is not valid gzip: {e}"))
            })?;
        out
    } else {
        bytes
    };
    let staged = to.with_extension("json.part");
    fs::write(&staged, &plain)
        .and_then(|_| fs::rename(&staged, to))
        .map_err(|e| {
            CliError::new("write_failed", format!("{}: {e}", to.display())).with_path("--out")
        })?;
    let _ = fs::remove_file(from);
    Ok(store::sha256_hex(&plain))
}

/// `simulate TARGET --cloud`: the target's revision simulated; `--out` gets `trace.json` and `result.json`.
pub fn cloud_simulate(a: &ExecutionArgs, ctx: &Ctx) -> CmdResult {
    refuse_extra(&a.extra)?;
    if a.rig.is_some() || a.preset.is_some() {
        return Err(CliError::new(
            "bad_value",
            "--rig and --preset apply to render, not simulate",
        ));
    }
    let target = target(&a.target, a.revision.as_deref())?;
    let c = cloud::client(ctx)?;
    let wait = a.wait || a.download || a.out.is_some();
    let (run, rev) = match submit(&c, ctx, target, "simulate", json!({}), a.max_cost, wait)? {
        Ok(done) => done,
        Err(dry) => return Ok(Outcome::ok(dry)),
    };
    let mut extra = json!({});
    if let Some(out) = a.out.as_deref().filter(|_| run["status"] == "succeeded") {
        let run_id = str_at(&run, "/id")?;
        let artifacts = cloud::run_artifacts(&c, run_id)?;
        let traces: Vec<&Value> = artifacts
            .iter()
            .filter(|t| t["name"].as_str().is_some_and(|n| n.starts_with("trace/")))
            .collect();
        let trace = pick_trace(&run, &traces)?.clone();
        let mut planned = vec![(".trace.download".to_owned(), trace.clone())];
        for art in &artifacts {
            let name = art["name"].as_str().unwrap_or_default();
            if !name.starts_with("trace/") {
                planned.push((name.to_owned(), art.clone()));
            }
        }
        fs::create_dir_all(out).map_err(|e| {
            CliError::new("write_failed", format!("{}: {e}", out.display())).with_path("--out")
        })?;
        let mut files = download_into(&c, out, planned)?;
        let sha = normalize_trace(&out.join(".trace.download"), &out.join("trace.json"))?;
        files[0] = json!({ "path": "trace.json", "sha256": sha, "artifact": trace["name"], "artifactSha256": trace["sha256"] });
        extra["out"] = json!(out);
        extra["artifacts"] = json!(files);
    }
    Ok(report(run, &rev, extra))
}

/// `preview frame|clip TARGET --cloud`: one camera (or the 2d top-down for a clip) rendered short, one file at `--out`.
pub fn cloud_preview(a: &PreviewArgs, clip: bool, ctx: &Ctx) -> CmdResult {
    refuse_extra(&a.extra)?;
    for path in [
        a.out.clone(),
        preview_manifest::sidecar(&a.out, ".render.json"),
        preview_manifest::sidecar(&a.out, ".runtime.json"),
        preview_manifest::sidecar(&a.out, ".preview.json"),
    ] {
        if path.exists() && !a.force {
            return Err(
                CliError::new("file_exists", format!("{} already exists", path.display()))
                    .with_path("--out")
                    .with_detail(json!({ "hint": "pass --force to overwrite" })),
            );
        }
    }
    let span = match (clip, a.seconds) {
        (true, s) => PreviewSpan::Clip {
            at: a.at,
            seconds: s.unwrap_or(3.0),
        },
        (false, None) => PreviewSpan::Frame { at: a.at },
        (false, Some(_)) => {
            return Err(
                CliError::new("bad_value", "--seconds applies to preview clip")
                    .with_path("--seconds"),
            )
        }
    };
    let (input, camera) = match a.mode {
        RenderMode::TwoD => {
            if a.camera.is_some() || a.rig.is_some() {
                return Err(CliError::new("bad_value", "a 2d preview is the top-down view: --rig and --camera apply to 3d and schematic"));
            }
            let seconds = match span {
                PreviewSpan::Clip { seconds, .. } => seconds,
                PreviewSpan::Frame { .. } => 1.0 / 20.0,
            };
            if !(a.at.is_finite() && a.at >= 0.0 && seconds.is_finite() && seconds > 0.0) {
                return Err(CliError::new(
                    "bad_value",
                    "--at must be ≥ 0 and --seconds positive",
                ));
            }
            let rig = json!({ "clip": { "startSeconds": a.at, "endSeconds": a.at + seconds } });
            let mut input = render_input(RenderMode::TwoD, a.preset.as_deref(), Some(rig))?;
            input["outputs"] = json!(if clip { "video" } else { "stills" });
            (input, None)
        }
        mode => {
            let rig_path = a.rig.as_deref().ok_or_else(|| {
                CliError::new("missing_argument", "a 3d preview needs --rig").with_path("--rig")
            })?;
            let camera = a.camera.clone().ok_or_else(|| {
                CliError::new("missing_argument", "a 3d preview needs --camera")
                    .with_path("--camera")
            })?;
            let rig = preview_rig(&crate::rig::read_file(rig_path, "--rig")?, &camera, span)?;
            let preset = match mode {
                RenderMode::ThreeD => Some(a.preset.as_deref().unwrap_or("showcase")),
                _ => a.preset.as_deref(),
            };
            let mut input = render_input(mode, preset, Some(rig))?;
            input["outputs"] = json!(if clip { "video" } else { "stills" });
            (input, Some(camera))
        }
    };
    let target = target(&a.target, a.revision.as_deref())?;
    let c = cloud::client(ctx)?;
    let (run, rev) = match submit(&c, ctx, target, "render", input, a.max_cost, true)? {
        Ok(done) => done,
        Err(dry) => return Ok(Outcome::ok(dry)),
    };
    if run["status"] != "succeeded" {
        return Ok(report(run, &rev, json!({})));
    }
    let run_id = str_at(&run, "/id")?;
    let wanted = |name: &str| match &camera {
        None if clip => name.ends_with("/video/topdown.mp4"),
        None => name.ends_with("/topdown.png"),
        Some(cam) if clip => name.ends_with(&format!("/video/{cam}.mp4")),
        Some(cam) => name.contains(&format!("/{cam}/")) && name.ends_with(".rgb.png"),
    };
    let artifacts = cloud::run_artifacts(&c, run_id)?;
    let artifact = artifacts
        .iter()
        .filter(|art| art["name"].as_str().is_some_and(wanted))
        .min_by(|x, y| x["name"].as_str().cmp(&y["name"].as_str()))
        .cloned()
        .ok_or_else(|| {
            CliError::new(
                "preview_output_missing",
                format!(
                    "run {run_id} returned no {}",
                    if clip { "video" } else { "frame" }
                ),
            )
            .with_detail(json!({ "created": rev.created }))
        })?;
    let scratch = a.out.with_extension("download");
    let got = cloud::download_artifact(&c, &artifact, &scratch)?;
    fs::rename(&scratch, &a.out).map_err(|e| {
        CliError::new(
            "write_failed",
            format!("cannot write {}: {e}", a.out.display()),
        )
        .with_path("--out")
    })?;
    let manifest = preview_manifest::publish(
        &c,
        &artifacts,
        &a.out,
        &artifact,
        &rev.source,
        a.mode.as_str(),
        if clip { "clip" } else { "frame" },
        run_id,
    )?;
    Ok(report(
        run,
        &rev,
        json!({
            "out": a.out,
            "kind": if clip { "clip" } else { "frame" },
            "mode": a.mode.as_str(),
            "camera": camera,
            "atSeconds": a.at,
            "seconds": if clip { json!(a.seconds.unwrap_or(3.0)) } else { Value::Null },
            "artifact": artifact["name"],
            "sha256": got["sha256"],
            "manifest": manifest,
        }),
    ))
}

#[derive(Debug, Parser)]
#[command(name = "scenario", no_binary_name = true)]
struct ScenarioCli {
    #[command(subcommand)]
    command: ScenarioCloudCommand,
}

#[derive(Debug, Subcommand)]
enum ScenarioCloudCommand {
    New(NewArgs),
    Apply(ApplyArgs),
    Check(CheckArgs),
}

#[derive(Debug, Args)]
struct NewArgs {
    #[arg(long, value_name = "MAP")]
    map: String,
    #[arg(long, short = 'o', value_name = "FILE")]
    out: PathBuf,
    #[arg(long, value_name = "NAME", default_value = "Untitled scenario")]
    name: String,
    #[arg(long, value_name = "S", default_value_t = 20.0)]
    clip_seconds: f64,
    #[arg(long, value_name = "TIME")]
    created_at: Option<String>,
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct ApplyArgs {
    #[arg(value_name = "FILE")]
    file: PathBuf,
    #[arg(long, value_name = "N")]
    expect_version: Option<u64>,
    #[arg(long, value_name = "SHA256")]
    expect_digest: Option<String>,
    #[arg(long, value_name = "TEXT")]
    transaction_label: Option<String>,
    #[command(subcommand)]
    operations: Operations,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Strictness {
    Errors,
    Warnings,
}

#[derive(Debug, Args)]
struct CheckArgs {
    #[arg(value_name = "FILE")]
    file: PathBuf,
    #[arg(long, value_enum)]
    strictness: Strictness,
    #[arg(long)]
    preview: bool,
}

/// `scenario new|apply|check … --cloud`: the local scenario grammar, the host's authoring operations,
/// the file kept under its compare-and-swap revision. `argv` starts at the subcommand.
pub fn cloud_scenario(argv: &[String], ctx: &Ctx) -> CmdResult {
    match argv.first().map(String::as_str) {
        Some("new" | "apply" | "check") => {}
        other => {
            return Err(CliError::new(
                "operation_unavailable",
                format!(
                    "scenario {} has no cloud form; run it without --cloud",
                    other.unwrap_or("")
                ),
            ))
        }
    }
    let parsed = ScenarioCli::try_parse_from(argv).map_err(|e| crate::parse_error(&e, argv))?;
    match parsed.command {
        ScenarioCloudCommand::New(a) => scenario_new(a, ctx),
        ScenarioCloudCommand::Apply(a) => scenario_apply(a, ctx),
        ScenarioCloudCommand::Check(a) => scenario_check(a, ctx),
    }
}

fn dry(request: Value) -> CmdResult {
    Ok(Outcome::ok(json!({ "dryRun": true, "requests": request })))
}

fn scenario_new(a: NewArgs, ctx: &Ctx) -> CmdResult {
    if a.out.exists() && !a.force {
        return Err(
            CliError::new("file_exists", format!("{} already exists", a.out.display()))
                .with_path("--out")
                .with_detail(json!({ "hint": "pass --force to overwrite" })),
        );
    }
    let mut body = json!({ "mapId": a.map, "name": a.name, "clipSeconds": a.clip_seconds });
    let logical_map = a.map.split('@').next().unwrap_or(&a.map);
    let pin = map_pin::request(
        &json!({"sourceMap":{"mapId":logical_map}}),
        ctx.map_version.as_deref(),
    )?;
    body.as_object_mut()
        .unwrap()
        .extend(pin.as_object().unwrap().clone());
    if let Some(t) = &a.created_at {
        body["createdAt"] = json!(t);
    }
    if ctx.dry_run {
        return dry(json!([{ "method": "POST", "path": "/drafts", "body": body }]));
    }
    let c = cloud::client(ctx)?;
    let receipt = c.fresh().post("/drafts", &body)?;
    if let Some(release) = body.get("mapReleaseDigest") {
        if receipt["mapPin"]["releaseDigest"] != *release {
            return Err(CliError::new(
                "map_pin_mismatch",
                "new hosted scenario differs from the installed map release",
            )
            .with_detail(json!({"draftId":receipt["documentId"]})));
        }
    }
    let mut document = receipt["content"].clone();
    let xodr = str_at(&receipt, "/mapPin/xodrSha256")?.to_owned();
    let map_id = str_at(&receipt, "/mapPin/mapId")?.to_owned();
    // The file pins the exact map version the host created it on, as `scenario pull` does.
    let pinned = document["sourceMap"]["xodrSha256"]
        .as_str()
        .map(str::to_owned);
    match pinned {
        None => document["sourceMap"]["xodrSha256"] = json!(xodr),
        Some(pinned) if pinned == xodr => {}
        Some(pinned) => {
            return Err(CliError::new(
                "map_pin_mismatch",
                "the host's new document pins another map digest",
            )
            .with_detail(json!({ "document": pinned, "mapPin": receipt["mapPin"] })))
        }
    }
    let revision = store::create(&a.out, &document, a.force)?;
    Ok(Outcome::ok(json!({
        "file": a.out.display().to_string(),
        "mapId": map_id,
        "xodrSha256": xodr,
        "clipSeconds": a.clip_seconds,
        "version": revision.version,
        "digest": revision.digest,
        "cloud": {
            "draftId": receipt["documentId"],
            "mapVersionId": receipt["mapPin"]["versionId"],
            "provisional": receipt["provisional"],
        },
    })))
}

fn scenario_apply(a: ApplyArgs, ctx: &Ctx) -> CmdResult {
    let locked = store::open(&a.file)?;
    let base = locked.base(a.expect_version, a.expect_digest.as_deref())?;
    let transaction = authoring_ops::transaction_value(
        base.version,
        &base.digest,
        a.transaction_label.as_deref(),
        a.operations.0,
    );
    let mut body = json!({
        "label": transaction["label"],
        "operations": transaction["operations"],
    });
    if ctx.dry_run {
        return Ok(Outcome::ok(
            json!({"dryRun":true,"file":a.file,"digest":base.digest,"transaction":transaction,"mapPin":map_pin::request(&locked.document,ctx.map_version.as_deref())?,"hint":"Syntax, file revision and release pins checked; no hosted map-aware validation or mutation performed."}),
        ));
    }
    let c = cloud::client(ctx)?;
    let caps = capabilities(&c)?;
    require_available(&caps, "hostOperations", &["apply"])?;
    let ids: Vec<&str> = body["operations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["type"].as_str())
        .collect();
    require_available(&caps, "operations", &ids)?;
    let receipt = import(
        &c,
        &a.file,
        &locked.document,
        &base.digest,
        true,
        ctx.map_version.as_deref(),
    )?;
    let draft_id = str_at(&receipt, "/documentId")?.to_owned();
    body["expectedVersion"] = receipt["draftVersion"].clone();
    body["expectedDigest"] = receipt["contentDigest"].clone();
    let result = c
        .fresh()
        .post(&draft_path(&draft_id, Some("transactions")), &body)?;
    let document = result["document"].clone();
    let digest = store::digest(&document)?;
    if result["digest"] != json!(digest) {
        return Err(CliError::new(
            "content_identity_mismatch",
            "the host's transaction result does not hash to its digest",
        )
        .with_detail(
            json!({ "host": result["digest"], "computed": digest, "draftId": draft_id }),
        ));
    }
    if document["sourceMap"] != locked.document["sourceMap"] {
        return Err(CliError::new(
            "map_pin_mismatch",
            "the transaction changed the document's map pin",
        )
        .with_detail(json!({ "draftId": draft_id })));
    }
    let committed = locked.commit(&base, &document, base.version + 1)?;
    Ok(Outcome::ok(json!({
        "document": document,
        "version": committed.version,
        "digest": committed.digest,
        "changedPaths": result["changedPaths"],
        "entityIds": result["entityIds"],
        "diagnostics": result["diagnostics"],
        "inverseDocument": result["inverseDocument"],
        "cloud": {
            "draftId": draft_id,
            "draftVersion": result["draftVersion"],
            "contentDigest": result["contentDigest"],
            "mapPin": result["mapPin"],
            "provisional": result["provisional"],
        },
    })))
}

fn scenario_check(a: CheckArgs, ctx: &Ctx) -> CmdResult {
    let (document, digest, recorded) = {
        let locked = store::open(&a.file)?;
        (locked.document, locked.digest, locked.recorded)
    };
    let (map_id, xodr) = source_map(&document, &a.file)?;
    let strict = a.strictness == Strictness::Warnings;
    let validate = json!({ "strict": strict, "preview": a.preview });
    if ctx.dry_run {
        return Ok(Outcome::ok(
            json!({"dryRun":true,"file":a.file,"digest":digest,"validation":validate,"mapPin":map_pin::request(&document,ctx.map_version.as_deref())?,"hint":"Release pins checked; no hosted validation or mutation performed."}),
        ));
    }
    let c = cloud::client(ctx)?;
    let receipt = import(
        &c,
        &a.file,
        &document,
        &digest,
        true,
        ctx.map_version.as_deref(),
    )?;
    let draft_id = str_at(&receipt, "/documentId")?.to_owned();
    let mut value = c.post(&draft_path(&draft_id, Some("validate")), &validate)?;
    let ok = value["ok"].as_bool() == Some(true);
    value["file"] = json!(a.file.display().to_string());
    value["mapId"] = json!(map_id);
    value["xodrSha256"] = json!(xodr);
    value["digest"] = json!(digest);
    value["version"] = json!(recorded.filter(|r| r.digest == digest).map(|r| r.version));
    value["cloud"] = json!({ "draftId": draft_id, "mapPin": receipt["mapPin"], "provisional": receipt["provisional"] });
    Ok(Outcome {
        value,
        exit: if ok { Exit::Ok } else { Exit::Findings },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_artifacts_land_where_the_local_render_writes_them() {
        let arts = vec![
            json!({"id": "art_1", "name": "render-3d/video/front.mp4"}),
            json!({"id": "art_2", "name": "render-2d/render.json"}),
            json!({"id": "art_3", "name": "notes/render-3d/x.json"}),
        ];
        let paths: Vec<String> = render_layout(arts).into_iter().map(|(p, _)| p).collect();
        assert_eq!(
            paths,
            ["video/front.mp4", "render.json", "notes/render-3d/x.json"]
        );
    }

    #[test]
    fn the_trace_is_the_one_the_run_names_else_the_only_one() {
        let a = json!({"name": "trace/aaa.trace.json.gz", "sha256": "1"});
        let b = json!({"name": "trace/bbb.trace.json.gz", "sha256": "2"});
        let run = json!({"result": {"traceSha256": "bbb"}});
        assert_eq!(pick_trace(&run, &[&a, &b]).unwrap()["name"], b["name"]);
        assert_eq!(pick_trace(&json!({}), &[&a]).unwrap()["name"], a["name"]);
        assert_eq!(
            pick_trace(&json!({}), &[&a, &b]).unwrap_err().code,
            "simulate_trace_ambiguous"
        );
    }

    #[test]
    fn a_file_target_with_a_revision_is_refused_and_a_missing_one_needs_a_revision() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.json");
        fs::write(&file, "{}").unwrap();
        assert_eq!(
            target(&file, Some("rev_1")).err().unwrap().code,
            "bad_value"
        );
        assert!(matches!(target(&file, None).unwrap(), Target::File(_)));
        let missing = Path::new("scn_123");
        assert_eq!(target(missing, None).err().unwrap().code, "file_not_found");
        assert!(matches!(
            target(missing, Some("rev_1")).unwrap(),
            Target::Hosted { .. }
        ));
    }
}
