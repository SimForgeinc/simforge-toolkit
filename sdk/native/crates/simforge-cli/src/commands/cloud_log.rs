//! Log ingestion on the cloud (`scenario_from_log`): `simforge import log FILE --cloud` and
//! `simforge scenarios create --log FILE` share this path.
//!
//! The file is uploaded once (checksum-bound PUT) as what it is: a ROS 2 MCAP lidar log
//! (recognized by its magic bytes) is an `mcap` upload, perceived on a GPU worker before it
//! is registered and imported; anything else is a `recording` upload (a
//! `simforge.recording/v1` or `simforge.log-scene/v1` file, JSON or gzipped). The run creates
//! the scenario in the workspace; its result is the scenario, the import report and, for an
//! MCAP log, the perceive method and its licence (internal use: the detector's weights are
//! CC BY-NC-SA 4.0).

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Parser};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::cloud::{self, Client};
use crate::contract::{CliError, CmdResult, Ctx, Outcome};

/// The MCAP file magic (`\x89MCAP0\r\n`).
const MCAP_MAGIC: &[u8] = b"\x89MCAP0\r\n";

/// Perceive options; they apply only to an MCAP log.
#[derive(Debug, Args, Default)]
pub struct PerceiveArgs {
    /// MCAP only: the lidar topic, when the log has several.
    #[arg(long, value_name = "TOPIC")]
    pub lidar_topic: Option<String>,
    /// MCAP only: the GNSS topic, when the log has several.
    #[arg(long, value_name = "TOPIC")]
    pub gnss_topic: Option<String>,
    /// MCAP only: the ego's box in metres, length,width,height (perceive's default otherwise).
    #[arg(
        long,
        value_name = "L,W,H",
        requires = "ego_rear_axle_to_centre",
        value_delimiter = ',',
        num_args = 3
    )]
    pub ego_dims: Option<Vec<f64>>,
    /// MCAP only: how far the ego box's centre is ahead of base_link (the rear axle), metres.
    #[arg(
        long,
        value_name = "METRES",
        requires = "ego_dims",
        allow_hyphen_values = true
    )]
    pub ego_rear_axle_to_centre: Option<f64>,
}

impl PerceiveArgs {
    fn given(&self) -> Option<&'static str> {
        if self.lidar_topic.is_some() {
            Some("--lidar-topic")
        } else if self.gnss_topic.is_some() {
            Some("--gnss-topic")
        } else if self.ego_dims.is_some() {
            Some("--ego-dims")
        } else {
            None
        }
    }
}

/// `simforge import log FILE --cloud ...`.
#[derive(Debug, Parser)]
pub struct ImportLogCloudArgs {
    /// The log: a recording (JSON, optionally gzipped) or a ROS 2 MCAP lidar log.
    #[arg(value_name = "RECORDING|LOG.mcap")]
    pub file: PathBuf,
    /// The map: a map version id, a name, `name@vN`, or `auto` (the recording's own, or the map an MCAP log's GNSS path lies in).
    #[arg(long, value_name = "NAME@VERSION|auto")]
    pub map: String,
    /// The scenario's name. Default: the file's name.
    #[arg(long)]
    pub name: Option<String>,
    /// Explicit maximum cloud cost in cents.
    #[arg(long, value_name = "CENTS")]
    pub max_cost: Option<u64>,
    /// Download the created scenario document, import report and Event Timeline here.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out: Option<PathBuf>,
    #[command(flatten)]
    pub perceive: PerceiveArgs,
}

fn upload_kind(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(MCAP_MAGIC) {
        "mcap"
    } else {
        "recording"
    }
}

/// The `scenario_from_log` input for an upload of `kind`; perceive options refuse a recording.
fn log_input(
    upload_id: &str,
    kind: &str,
    map: &str,
    name: &str,
    p: &PerceiveArgs,
) -> Result<Value, CliError> {
    let mut input = json!({ "uploadId": upload_id, "mapId": map, "name": name });
    if kind != "mcap" {
        if let Some(flag) = p.given() {
            return Err(CliError::new(
                "bad_value",
                format!("{flag} applies to an MCAP log; this file is a recording"),
            )
            .with_path(flag));
        }
        return Ok(input);
    }
    if let Some(t) = &p.lidar_topic {
        input["lidarTopic"] = json!(t);
    }
    if let Some(t) = &p.gnss_topic {
        input["gnssTopic"] = json!(t);
    }
    if let (Some(d), Some(r)) = (&p.ego_dims, p.ego_rear_axle_to_centre) {
        if d.len() != 3 || d.iter().any(|v| !v.is_finite() || *v <= 0.0) || !r.is_finite() {
            return Err(CliError::new(
                "bad_value",
                "--ego-dims takes three positive lengths in metres",
            )
            .with_path("--ego-dims"));
        }
        input["ego"] =
            json!({ "dims": { "l": d[0], "w": d[1], "h": d[2] }, "rearAxleToCentreM": r });
    }
    Ok(input)
}

/// Upload `file` as its kind; returns the upload id and the kind.
fn upload(c: &Client, file: &Path) -> Result<(String, &'static str), CliError> {
    let bytes = fs::read(file).map_err(|e| {
        CliError::new("file_unreadable", format!("{}: {e}", file.display()))
            .with_path(file.display().to_string())
    })?;
    let kind = upload_kind(&bytes);
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let granted = c.post(
        "/uploads",
        &json!({ "kind": kind, "sha256": sha256, "sizeBytes": bytes.len() }),
    )?;
    let field = |k: &str| {
        granted
            .get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| CliError::new("invalid_api_response", format!("{k} missing")))
    };
    let (id, url) = (field("uploadId")?.to_owned(), field("uploadUrl")?);
    let empty = Map::new();
    let headers = granted
        .get("uploadHeaders")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    c.upload(url, &bytes, headers)?;
    Ok((id, kind))
}

/// One log to ingest: the file, its map and name, the cost cap, perceive options and where to download.
pub(crate) struct Ingest<'a> {
    pub file: &'a Path,
    pub map: &'a str,
    pub name: &'a str,
    pub max_cost: Option<u64>,
    pub perceive: &'a PerceiveArgs,
    pub out: Option<&'a Path>,
}

/// Upload the log, submit `scenario_from_log` and wait for the scenario. A dry run uploads too:
/// the host prices and validates the import against the stored upload.
pub(crate) fn ingest(c: &Client, ctx: &Ctx, job: Ingest) -> CmdResult {
    let Ingest {
        file,
        map,
        name,
        max_cost,
        perceive,
        out,
    } = job;
    if !ctx.dry_run && max_cost.is_none() {
        return Err(CliError::new(
            "max_cost_required",
            "cloud execution requires --max-cost CENTS",
        )
        .with_path("--max-cost"));
    }
    let (upload_id, kind) = upload(c, file)?;
    let input = log_input(&upload_id, kind, map, name, perceive)?;
    let created = c.post(
        "/runs",
        &cloud::run_request("scenario_from_log", input, ctx.dry_run, max_cost),
    )?;
    if ctx.dry_run {
        return Ok(Outcome::ok(created));
    }
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("invalid_api_response", "run id missing"))?;
    let run = cloud::wait_public(c, id)?;
    let succeeded = run.get("status").and_then(Value::as_str) == Some("succeeded");
    let result = run.get("result").filter(|r| !r.is_null());
    match (succeeded, result) {
        (true, Some(result)) => {
            let mut value = result.clone();
            value["runId"] = json!(id);
            value["upload"] = json!({ "id": upload_id, "kind": kind });
            if let Some(out) = out {
                value["out"] = json!(out);
                value["artifacts"] = cloud::download_kinds(
                    c,
                    id,
                    out,
                    &["scenario_document", "import_report", "event_timeline"],
                )?;
            }
            Ok(Outcome::ok(value))
        }
        // A run that did not succeed reports itself (its failed job names the code) as findings.
        _ => Ok(Outcome::findings(run)),
    }
}

/// `simforge import log FILE --cloud`: `argv` is what follows `import`.
pub fn import_cloud(argv: &[String], ctx: &Ctx) -> CmdResult {
    if argv.first().map(String::as_str) != Some("log") {
        return Err(CliError::new(
            "operation_unavailable",
            "only `import log` has a cloud implementation; use the local runtime for the other imports",
        ));
    }
    let a = ImportLogCloudArgs::try_parse_from(
        std::iter::once("import log".to_owned()).chain(argv[1..].iter().cloned()),
    )
    .map_err(|error| crate::parse_error(&error, &ctx.argv))?;
    let name = a.name.clone().unwrap_or_else(|| default_name(&a.file));
    let c = cloud::client(ctx)?;
    ingest(
        &c,
        ctx,
        Ingest {
            file: &a.file,
            map: &a.map,
            name: &name,
            max_cost: a.max_cost,
            perceive: &a.perceive,
            out: a.out.as_deref(),
        },
    )
}

/// A file's name without its log/recording extensions (`drive.recording.json.gz` → `drive`).
fn default_name(file: &Path) -> String {
    let base = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "log".to_owned());
    let mut name = base.as_str();
    for ext in [".gz", ".json", ".mcap", ".recording", ".rec"] {
        name = name.strip_suffix(ext).unwrap_or(name);
    }
    let name = if name.is_empty() { base.as_str() } else { name };
    name.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_mcap_log_is_recognized_by_its_magic() {
        assert_eq!(upload_kind(b"\x89MCAP0\r\n\x01rest"), "mcap");
        assert_eq!(
            upload_kind(b"{\"schema\":\"simforge.recording/v1\"}"),
            "recording"
        );
        assert_eq!(upload_kind(&[0x1f, 0x8b, 8]), "recording");
    }

    #[test]
    fn perceive_options_go_into_an_mcap_input_and_refuse_a_recording() {
        let p = PerceiveArgs {
            lidar_topic: Some("/lidar/top".into()),
            gnss_topic: None,
            ego_dims: Some(vec![4.8, 1.9, 1.6]),
            ego_rear_axle_to_centre: Some(1.4),
        };
        let input = log_input("upl_1", "mcap", "auto", "drive", &p).unwrap();
        assert_eq!(
            input,
            json!({"uploadId":"upl_1","mapId":"auto","name":"drive","lidarTopic":"/lidar/top",
                   "ego":{"dims":{"l":4.8,"w":1.9,"h":1.6},"rearAxleToCentreM":1.4}})
        );
        let refused = log_input("upl_1", "recording", "auto", "drive", &p).unwrap_err();
        assert_eq!(refused.code, "bad_value");
        assert_eq!(refused.path.as_deref(), Some("--lidar-topic"));
        assert_eq!(
            log_input(
                "upl_1",
                "recording",
                "rfs@v5",
                "d",
                &PerceiveArgs::default()
            )
            .unwrap(),
            json!({"uploadId":"upl_1","mapId":"rfs@v5","name":"d"})
        );
    }

    #[test]
    fn the_default_name_is_the_files_stem() {
        assert_eq!(
            default_name(Path::new("/x/drive.recording.json.gz")),
            "drive"
        );
        assert_eq!(default_name(Path::new("site-3.mcap")), "site-3");
        assert_eq!(default_name(Path::new(".json")), ".json");
    }

    #[test]
    fn import_log_cloud_parses_the_local_shape() {
        let a = ImportLogCloudArgs::try_parse_from([
            "import log",
            "drive.mcap",
            "--map",
            "auto",
            "--max-cost",
            "90",
            "--gnss-topic",
            "/gnss",
            "--out",
            "o",
        ])
        .unwrap();
        assert_eq!(a.map, "auto");
        assert_eq!(a.max_cost, Some(90));
        assert_eq!(a.perceive.gnss_topic.as_deref(), Some("/gnss"));
        assert_eq!(a.out, Some(PathBuf::from("o")));
    }
}
