//! `simforge datasets`: browse datasets and export them
//! (`simforge.dataset-export/v1`, sdk/docs/engineering/dataset-export.md).
//!
//! `export` submits the same job the Studio export sheet does
//! (`POST /api/v1/exports`), watches it on one updating line (stage, counts,
//! bytes, the ETA the API measures) and downloads it; `pull` waits for and
//! downloads an export by id, resuming whatever an earlier pull left;
//! `status` reports one export; `retry` decides about scenarios an export
//! could not package; `verify` checks a downloaded export offline.

use crate::contract::{CliError, CmdResult, Ctx, Outcome};
use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use super::cloud::{self, seg, Client, IdArgs, ListArgs};
use super::datasets_verify;
use super::fetch::{fetch, Target};

const FACETS: &[&str] = &[
    "map",
    "category",
    "scope",
    "mode",
    "variant",
    "verdict",
    "tier",
    "grade",
    "simulation",
    "licence",
    "split",
    "source",
    "weather",
    "timeOfDay",
    "roadType",
    "render",
    "behaviour",
    "tag",
];
const PRESETS: &[&str] = &["training", "showcase", "schematic"];
const MODALITIES: &[&str] = &["rgb", "depth", "semantic", "instance", "lidar", "radar"];
const DOWNLOAD_THREADS: usize = 6;

#[derive(Debug, Subcommand)]
pub enum DatasetsCommand {
    /// List the hosted datasets of your workspace.
    List(ListArgs),
    /// One hosted dataset.
    Show(IdArgs),
    /// Export a dataset, a filter of it or chosen scenarios: submit, watch, download.
    Export(ExportArgs),
    /// Wait for an export and download it (resumes an interrupted download).
    Pull(PullArgs),
    /// One export: stage, counts, bytes and the measured ETA.
    Status(IdArgs),
    /// Package the scenarios an export left out again.
    Retry(RetryArgs),
    /// Check a downloaded export against its SHA256SUMS and manifest (offline).
    Verify(VerifyArgs),
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// The dataset id.
    pub dataset: String,
    /// Library filters as `key:value` (repeat a key or comma-separate values), e.g. 'grade:A tag:cut-in'.
    #[arg(long)]
    pub filter: Option<String>,
    /// Free-text search, as in the library's search box.
    #[arg(long)]
    pub q: Option<String>,
    /// Only the scenarios of one recorded drive (its recording digest or source id).
    #[arg(long)]
    pub source_log: Option<String>,
    /// Exactly these scenarios (document ids, comma-separated).
    #[arg(long, value_delimiter = ',', conflicts_with_all = ["filter", "q", "source_log"])]
    pub ids: Vec<String>,
    /// Beside the scenarios: renders[:training|showcase|schematic], labels, gt, recordings
    /// (default renders:training; `scenarios` alone for nothing else).
    #[arg(long, value_delimiter = ',')]
    pub include: Vec<String>,
    /// Keep only these render modalities: rgb, depth, semantic, instance, lidar, radar.
    #[arg(long, value_delimiter = ',')]
    pub passes: Vec<String>,
    /// Also make ZIP volumes, and download those instead of the folder.
    #[arg(long)]
    pub zip: bool,
    /// Where to download the export (required unless --detach).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Submit and print the export without waiting for it.
    #[arg(long)]
    pub detach: bool,
}

#[derive(Debug, Args)]
pub struct PullArgs {
    /// The export id (`run_…`).
    pub id: String,
    /// Directory to download the export into.
    #[arg(long)]
    pub out: PathBuf,
    /// Download the files of a ZIP export as a folder instead of its volumes.
    #[arg(long)]
    pub folder: bool,
}

#[derive(Debug, Args)]
pub struct RetryArgs {
    /// The export id (`run_…`).
    pub id: String,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// The downloaded export directory.
    pub dir: PathBuf,
    /// Also verify every scenario package (local build).
    #[arg(long)]
    pub packages: bool,
}

pub fn run(command: DatasetsCommand, ctx: &Ctx) -> CmdResult {
    match command {
        DatasetsCommand::Verify(a) => datasets_verify::verify(&a.dir, a.packages),
        command => {
            let c = cloud::client(ctx)?;
            let result = match command {
                DatasetsCommand::List(a) => return cloud::page(&c, "/datasets", &a),
                DatasetsCommand::Show(a) => {
                    return Ok(Outcome::ok(c.get(&format!("/datasets/{}", seg(&a.id)))?))
                }
                DatasetsCommand::Export(a) => export(&c, a, ctx),
                DatasetsCommand::Pull(a) => pull(&c, &a.id, &a.out, a.folder),
                DatasetsCommand::Status(a) => {
                    c.get(&format!("/exports/{}", seg(&a.id))).map(Outcome::ok)
                }
                DatasetsCommand::Retry(a) => c
                    .post(
                        &format!("/exports/{}/retry", seg(&a.id)),
                        &json!({"mode": "retry"}),
                    )
                    .map(Outcome::ok),
                DatasetsCommand::Verify(_) => unreachable!("handled above"),
            };
            result.map_err(|e| unavailable(&c, e))
        }
    }
}

/// A host without cloud runs answers every `/exports` route 404, as it answers
/// `/health`: say so by name instead of reporting a missing export.
fn unavailable(c: &Client, e: CliError) -> CliError {
    if e.code != "not_found" {
        return e;
    }
    match c.get("/health") {
        Err(health) if health.code == "not_found" => CliError::new(
            "exports_unavailable",
            "cloud runs are not enabled on this host, and dataset exports run as cloud runs",
        ),
        _ => e,
    }
}

fn invalid(code: &str, path: &str, reason: String) -> CliError {
    CliError::new(code, reason).with_path(path)
}

/// `--filter 'grade:A,B tag:cut-in'` → the library query's `filters`.
fn parse_filter(text: &str) -> Result<Map<String, Value>, CliError> {
    let mut filters: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for token in text.split_whitespace() {
        let (key, values) = token.split_once(':').ok_or_else(|| {
            invalid(
                "filter_invalid",
                "--filter",
                format!("{token:?} is not key:value"),
            )
        })?;
        if !FACETS.contains(&key) {
            return Err(invalid(
                "filter_invalid",
                "--filter",
                format!("unknown filter {key:?}; one of {}", FACETS.join(", ")),
            ));
        }
        let list = filters.entry(key.to_owned()).or_default();
        for value in values.split(',').filter(|v| !v.is_empty()) {
            list.push(value.to_owned());
        }
    }
    Ok(filters.into_iter().map(|(k, v)| (k, json!(v))).collect())
}

fn include(tokens: &[String], passes: &[String]) -> Result<Value, CliError> {
    let tokens: Vec<&str> = if tokens.is_empty() {
        vec!["renders:training"]
    } else {
        tokens.iter().map(String::as_str).collect()
    };
    let (mut presets, mut labels, mut gt, mut recordings) = (Vec::new(), false, false, false);
    for token in tokens {
        match token.split_once(':') {
            Some(("renders", preset)) if PRESETS.contains(&preset) => presets.push(preset),
            None if token == "renders" => presets.push("training"),
            None if token == "labels" => labels = true,
            None if token == "gt" => gt = true,
            None if token == "recordings" => recordings = true,
            None if token == "scenarios" => {}
            _ => {
                return Err(invalid(
                    "include_invalid",
                    "--include",
                    format!("{token:?}: use renders[:training|showcase|schematic], labels, gt, recordings or scenarios"),
                ))
            }
        }
    }
    for pass in passes {
        if !MODALITIES.contains(&pass.as_str()) {
            return Err(invalid(
                "passes_invalid",
                "--passes",
                format!("{pass:?}: one of {}", MODALITIES.join(", ")),
            ));
        }
    }
    presets.sort_by_key(|p| PRESETS.iter().position(|q| q == p));
    presets.dedup();
    Ok(json!({
        "renders": { "presets": presets, "modalities": if passes.is_empty() { Value::Null } else { json!(passes) } },
        "labels": labels,
        "groundTruth": gt,
        "recordings": recordings,
    }))
}

fn scope(a: &ExportArgs) -> Result<Value, CliError> {
    if !a.ids.is_empty() {
        return Ok(json!({"kind": "documents", "datasetId": a.dataset, "documentIds": a.ids}));
    }
    if a.filter.is_none() && a.q.is_none() && a.source_log.is_none() {
        return Ok(json!({"kind": "dataset", "datasetId": a.dataset}));
    }
    Ok(json!({
        "kind": "query",
        "datasetId": a.dataset,
        "query": {
            "q": a.q.clone().unwrap_or_default(),
            "filters": parse_filter(a.filter.as_deref().unwrap_or(""))?,
            "sourceLogId": a.source_log,
        },
    }))
}

fn export(c: &Client, a: ExportArgs, ctx: &Ctx) -> CmdResult {
    if a.out.is_none() && !a.detach && !ctx.dry_run {
        return Err(invalid(
            "missing_argument",
            "--out",
            "--out is required to download the export (or pass --detach)".into(),
        ));
    }
    let body = json!({
        "scope": scope(&a)?,
        "include": include(&a.include, &a.passes)?,
        "format": if a.zip { "zip" } else { "folder" },
        "dryRun": ctx.dry_run,
    });
    let created = c.post("/exports", &body)?;
    if ctx.dry_run || a.detach {
        return Ok(Outcome::ok(created));
    }
    let id = created["id"]
        .as_str()
        .ok_or_else(|| CliError::new("invalid_api_response", "export id missing"))?;
    pull(c, id, a.out.as_deref().expect("checked above"), false)
}

// ------------------------------------------------------------------ the one updating line

struct Line {
    enabled: bool,
}

impl Line {
    fn new() -> Self {
        Self {
            enabled: std::io::stderr().is_terminal(),
        }
    }
    fn show(&self, text: &str) {
        if self.enabled {
            let mut e = std::io::stderr();
            let _ = write!(e, "\r\x1b[2K{text}"); // fallback-ok: progress text only
            let _ = e.flush();
        }
    }
    fn done(&self) {
        if self.enabled {
            let _ = writeln!(std::io::stderr()); // fallback-ok: progress text only
        }
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(crate) fn human_bytes(n: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

fn eta(seconds: Option<f64>) -> String {
    match seconds {
        None => "estimating…".into(),
        Some(s) if s < 90.0 => "about a minute left".into(),
        Some(s) if s < 3600.0 * 1.5 => format!("about {} min left", (s / 60.0).round()),
        Some(s) => format!("about {:.1} h left", s / 3600.0),
    }
}

/// `Packaging · 1,240 / 5,000 scenarios · 38 GB of 120 GB · about 25 min left`
fn status_line(e: &Value) -> String {
    let p = &e["progress"];
    let stage = e["stage"].as_str().unwrap_or("");
    let word = match stage {
        "preparing" => "Preparing",
        "packaging" => "Packaging",
        "finalizing" => "Finalizing",
        "zipping" => "Zipping",
        "ready" => "Ready",
        "failed" => "Failed",
        _ => "Cancelled",
    };
    let scenarios = format!(
        "{} / {} scenarios",
        thousands(p["scenarios"]["done"].as_u64().unwrap_or(0)),
        thousands(p["scenarios"]["total"].as_u64().unwrap_or(0))
    );
    let bytes = |v: &Value| {
        format!(
            "{} of {}",
            human_bytes(v["done"].as_f64().unwrap_or(0.0)),
            human_bytes(v["total"].as_f64().unwrap_or(0.0))
        )
    };
    let pending = p["scenarios"]["pending"].as_u64().unwrap_or(0);
    let preparing = if pending > 0 && e["status"] == "running" {
        format!(" · preparing {}", thousands(pending))
    } else {
        String::new()
    };
    let middle = if stage == "zipping" {
        bytes(&p["zip"])
    } else {
        format!("{scenarios}{preparing} · {}", bytes(&p["bytes"]))
    };
    if e["status"] == "running" {
        format!("{word} · {middle} · {}", eta(p["etaSeconds"].as_f64()))
    } else {
        format!("{word} · {middle}")
    }
}

/// Follow an export until it stops running.
fn watch(c: &Client, id: &str, line: &Line) -> Result<Value, CliError> {
    let mut delay = Duration::from_secs(1);
    loop {
        let e = c.get(&format!("/exports/{}", seg(id)))?;
        line.show(&status_line(&e));
        if e["status"] != "running" {
            line.done();
            return Ok(e);
        }
        thread::sleep(delay);
        delay = (delay * 3 / 2).min(Duration::from_secs(10));
    }
}

fn not_ready(e: &Value) -> CliError {
    match e["status"].as_str() {
        Some("failed") => CliError::findings(
            e["error"]["code"].as_str().unwrap_or("export_failed"),
            e["error"]["reason"]
                .as_str()
                .unwrap_or("the export failed")
                .to_owned(),
        ),
        _ => CliError::findings("export_cancelled", "the export was cancelled"),
    }
}

// ------------------------------------------------------------------ download

struct Page {
    cursor: Option<String>,
    files: Vec<Value>,
}

fn files_url(id: &str, set: &str, cursor: Option<&str>) -> String {
    let mut p = format!("/exports/{}/files?set={set}&limit=1000", seg(id));
    if let Some(c) = cursor {
        p.push_str(&format!("&cursor={}", seg(c)));
    }
    p
}

fn pull(c: &Client, id: &str, out: &Path, folder: bool) -> CmdResult {
    let line = Line::new();
    let e = watch(c, id, &line)?;
    if e["status"] != "ready" {
        return Err(not_ready(&e));
    }
    let set = if e["format"] == "zip" && !folder {
        "zip"
    } else {
        "folder"
    };
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = c.get(&files_url(id, set, cursor.as_deref()))?;
        let files = page["files"].as_array().cloned().unwrap_or_default();
        pages.push(Page {
            cursor: cursor.clone(),
            files,
        });
        match page["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    let jobs: Vec<(usize, Value)> = pages
        .iter()
        .enumerate()
        .flat_map(|(i, p)| p.files.iter().map(move |f| (i, f.clone())))
        .collect();
    let total: u64 = jobs
        .iter()
        .map(|(_, f)| f["size"].as_u64().unwrap_or(0))
        .sum();
    let done = AtomicU64::new(0);
    let finished = AtomicUsize::new(0);
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<CliError>> = Mutex::new(None);
    let started = Instant::now();
    thread::scope(|s| {
        for _ in 0..DOWNLOAD_THREADS {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= jobs.len() || failure.lock().unwrap().is_some() {
                    return;
                }
                let (page, file) = &jobs[i];
                let path = file["path"].as_str().unwrap_or_default();
                let result = download_one(c, id, set, &pages[*page], file, out, &|n| {
                    done.fetch_add(n, Ordering::Relaxed);
                });
                match result {
                    Ok(()) => {
                        finished.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        *failure.lock().unwrap() = Some(e.with_path(path));
                        return;
                    }
                }
            });
        }
        while finished.load(Ordering::Relaxed) < jobs.len() && failure.lock().unwrap().is_none() {
            let bytes = done.load(Ordering::Relaxed) as f64;
            let elapsed = started.elapsed().as_secs_f64();
            let left = if elapsed > 3.0 && bytes > 0.0 {
                Some((total as f64 - bytes) / (bytes / elapsed))
            } else {
                None
            };
            line.show(&format!(
                "Downloading · {} / {} files · {} of {} · {}",
                thousands(finished.load(Ordering::Relaxed) as u64),
                thousands(jobs.len() as u64),
                human_bytes(bytes),
                human_bytes(total as f64),
                eta(left)
            ));
            thread::sleep(Duration::from_millis(250));
        }
    });
    line.done();
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    let verified = if set == "folder" {
        Some(datasets_verify::verify(out, false)?.value)
    } else {
        None
    };
    let warnings = excluded_warnings(&e, id);
    Ok(Outcome::ok(json!({
        "export": e,
        "out": out,
        "set": set,
        "files": jobs.len(),
        "bytes": total,
        "verified": verified,
        "warnings": warnings,
    })))
}

/// A ready export that left scenarios out is a success with a warning: they
/// are listed in its manifest, and `simforge datasets retry` packages them again.
fn excluded_warnings(e: &Value, id: &str) -> Value {
    let count = e["failures"]["count"].as_u64().unwrap_or(0);
    if count == 0 {
        return json!([]);
    }
    json!([{
        "code": "export_scenarios_excluded",
        "message": format!(
            "{count} scenarios couldn't be packaged and were left out (listed in the manifest); `simforge datasets retry {id}` packages them again"
        ),
        "detail": e["failures"],
    }])
}

/// Download one listed file; a refused URL is renewed by asking for its page again.
fn download_one(
    c: &Client,
    id: &str,
    set: &str,
    page: &Page,
    file: &Value,
    out: &Path,
    progress: &dyn Fn(u64),
) -> Result<(), CliError> {
    let path = file["path"]
        .as_str()
        .ok_or_else(|| CliError::new("invalid_api_response", "file path missing"))?;
    let sha256 = file["sha256"]
        .as_str()
        .ok_or_else(|| CliError::new("invalid_api_response", "file sha256 missing"))?;
    let size = file["size"]
        .as_u64()
        .ok_or_else(|| CliError::new("invalid_api_response", "file size missing"))?;
    let dest = out.join(cloud::safe(path)?);
    let mut first = file["url"].as_str().map(str::to_owned);
    let mut url = || -> Result<String, CliError> {
        if let Some(u) = first.take() {
            return Ok(u);
        }
        let fresh = c.get(&files_url(id, set, page.cursor.as_deref()))?;
        fresh["files"]
            .as_array()
            .and_then(|files| files.iter().find(|f| f["path"] == path))
            .and_then(|f| f["url"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                CliError::new(
                    "export_file_unlisted",
                    format!("{path} is no longer listed"),
                )
            })
    };
    fetch(
        &mut url,
        &Target {
            path: &dest,
            sha256,
            size,
        },
        &mut |n| progress(n),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_includes_parse_to_the_api_shapes() {
        let filters = parse_filter("grade:A,B tag:cut-in tag:night").unwrap();
        assert_eq!(
            Value::Object(filters),
            json!({"grade": ["A", "B"], "tag": ["cut-in", "night"]})
        );
        assert_eq!(
            parse_filter("colour:red").unwrap_err().code,
            "filter_invalid"
        );
        assert_eq!(parse_filter("grade").unwrap_err().code, "filter_invalid");
        assert_eq!(
            include(&[], &[]).unwrap(),
            json!({"renders": {"presets": ["training"], "modalities": null}, "labels": false, "groundTruth": false, "recordings": false})
        );
        let all = include(
            &[
                "renders:schematic".into(),
                "renders".into(),
                "gt".into(),
                "labels".into(),
                "recordings".into(),
            ],
            &["rgb".into()],
        )
        .unwrap();
        assert_eq!(
            all["renders"],
            json!({"presets": ["training", "schematic"], "modalities": ["rgb"]})
        );
        assert_eq!(
            include(&["scenarios".into()], &[]).unwrap()["renders"]["presets"],
            json!([])
        );
        assert_eq!(
            include(&["xosc".into()], &[]).unwrap_err().code,
            "include_invalid"
        );
        assert_eq!(
            include(&[], &["thermal".into()]).unwrap_err().code,
            "passes_invalid"
        );
    }
}
