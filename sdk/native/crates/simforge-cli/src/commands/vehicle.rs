//! `simforge vehicle`: custom vehicle models.
//!
//! `validate` checks a `.glb` against the vehicle model contract
//! (`simforge_glb::vehicle`, `docs/vehicle-models.md`) without a renderer, so
//! both builds carry it: a model is checked before it is imported.
//!
//! `list` names every vehicle a scenario can bind by `catalogId`. The cloud
//! build reads `GET /api/v1/vehicles`: the shared catalog's and the
//! workspace's own. The local build lists this binary's built-in catalog.
//!
//! `import` (signed in) validates a model here, uploads it and stores it as
//! the workspace's own `workspace.<id>`, private to it; the server
//! re-validates it with the same checks.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::contract::{CliError, CmdResult, Ctx, Outcome};
use simforge_glb::vehicle::{self, Expect, VehicleClass};
use simforge_glb::Glb;

#[derive(Debug, Subcommand)]
pub enum VehicleCommand {
    /// Check a .glb against the vehicle model contract (frame, size, wheel and body nodes, triangle budget, materials, textures); exit 2 on any error.
    Validate(ValidateArgs),
    /// The vehicles a scenario can bind by catalogId: the shared catalog's and, signed in, the workspace's own (`workspace.<id>`).
    List(ListArgs),
    /// Validate a .glb here, upload it and store it as the workspace's own `workspace.<id>` (private to the workspace); the licence and attribution travel with every render.
    Import(ImportArgs),
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// The model, a binary glTF (.glb).
    #[arg(value_name = "MODEL.GLB")]
    pub model: PathBuf,
    /// Its id within the workspace: scenarios bind it as `workspace.<id>` (lowercase letters, digits, `_`, `-`; at most 48).
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// Its licence: an SPDX id (`CC-BY-4.0`) or `LicenseRef-<name>` with --license-text.
    #[arg(long, value_name = "SPDX")]
    pub license: String,
    /// The licence's text, for a `LicenseRef-*` licence.
    #[arg(long, value_name = "FILE")]
    pub license_text: Option<PathBuf>,
    /// The credit the licence asks for (author, title, link).
    #[arg(long, value_name = "TEXT")]
    pub attribution: String,
    /// Where the model came from (URL or text).
    #[arg(long, value_name = "TEXT")]
    pub source: Option<String>,
    /// Its display name. Default: the id.
    #[arg(long, value_name = "TEXT")]
    pub name: Option<String>,
    /// The road-vehicle class it drives as: car, van, truck or bus.
    #[arg(long, value_name = "CLASS", default_value = "car")]
    pub class: String,
    /// Its real length, width and height in metres; the model must match within 10%.
    #[arg(long, value_name = "L,W,H")]
    pub dims: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// The model, a binary glTF (.glb).
    #[arg(value_name = "MODEL.GLB")]
    pub model: PathBuf,
    /// The road-vehicle class it will drive as: car, van, truck or bus (sets the plausible size).
    #[arg(long, value_name = "CLASS", default_value = "car")]
    pub class: String,
    /// Its real length, width and height in metres (`4.6,1.85,1.45`); the model must match within 10%.
    #[arg(long, value_name = "L,W,H")]
    pub dims: Option<String>,
}

pub fn run(command: VehicleCommand, ctx: &Ctx) -> CmdResult {
    match command {
        VehicleCommand::Validate(args) => validate(args),
        VehicleCommand::List(_) => list(ctx),
        VehicleCommand::Import(args) => import(args, ctx),
    }
}

/// The model checked here, the same way `validate` does: an invalid model never leaves the machine.
fn import(args: ImportArgs, ctx: &Ctx) -> CmdResult {
    let class = parse_class(&args.class)?;
    let dims = args.dims.as_deref().map(parse_dims).transpose()?;
    let bytes = std::fs::read(&args.model).map_err(|e| {
        CliError::new("file_unreadable", format!("{}: {e}", args.model.display()))
            .with_path(args.model.display().to_string())
    })?;
    let glb = Glb::parse(&bytes).map_err(|reason| {
        CliError::findings(
            "vehicle_glb_unreadable",
            format!("{}: {reason}", args.model.display()),
        )
    })?;
    let report = vehicle::validate(&glb, Expect { class, dims });
    if !report.ok {
        return Ok(Outcome::findings(
            json!({ "model": args.model, "report": report }),
        ));
    }
    let license_text = args
        .license_text
        .as_ref()
        .map(|p| {
            std::fs::read_to_string(p).map_err(|e| {
                CliError::new("file_unreadable", format!("{}: {e}", p.display()))
                    .with_path(p.display().to_string())
            })
        })
        .transpose()?;
    let mut request = json!({
        "id": args.id,
        "vehicleClass": class.as_str(),
        "license": args.license,
        "attribution": args.attribution,
    });
    for (key, value) in [
        ("name", args.name.map(Value::from)),
        ("dims", dims.map(|d| json!(d))),
        ("licenseText", license_text.map(Value::from)),
        ("source", args.source.map(Value::from)),
    ] {
        if let Some(value) = value {
            request[key] = value;
        }
    }
    let client = crate::commands::cloud::client(ctx)?;
    let vehicle = upload_and_store(
        &bytes,
        &report.sha256,
        request,
        |path, body| client.post(path, body),
        |url, body, headers| client.upload(url, body, headers),
    )?;
    Ok(Outcome::ok(json!({
        "schema": "simforge.vehicle-import/v1",
        "vehicle": vehicle,
        "report": report,
    })))
}

/// `POST /uploads` (kind `vehicle-model`), the PUT it grants, then
/// `POST /vehicles` naming the upload: the stored vehicle.
fn upload_and_store(
    bytes: &[u8],
    sha256: &str,
    mut request: Value,
    mut post: impl FnMut(&str, &Value) -> Result<Value, CliError>,
    put: impl FnOnce(&str, &[u8], &serde_json::Map<String, Value>) -> Result<(), CliError>,
) -> Result<Value, CliError> {
    let grant = post(
        "/uploads",
        &json!({ "kind": "vehicle-model", "sha256": sha256, "sizeBytes": bytes.len() }),
    )?;
    let field = |key: &str| {
        grant[key].as_str().map(str::to_owned).ok_or_else(|| {
            CliError::new(
                "invalid_api_response",
                format!("the upload grant has no {key}"),
            )
        })
    };
    let (upload_id, url) = (field("uploadId")?, field("uploadUrl")?);
    let headers = grant["uploadHeaders"].as_object().cloned().ok_or_else(|| {
        CliError::new(
            "invalid_api_response",
            "the upload grant has no uploadHeaders",
        )
    })?;
    put(&url, bytes, &headers)?;
    request["uploadId"] = Value::from(upload_id);
    post("/vehicles", &request)
}

/// The workspace's vehicles from the API, every page.
fn list(ctx: &Ctx) -> CmdResult {
    if !ctx.cloud {
        return crate::runtime::command(&["vehicle".into(), "list".into()]);
    }
    let client = crate::commands::cloud::client(ctx)?;
    let vehicles = every_page(|path| client.get(path))?;
    Ok(Outcome::ok(
        json!({ "schema": "simforge.vehicle-list/v1", "vehicles": vehicles }),
    ))
}

/// Follow `nextCursor` from the first `/vehicles` page to the last; a page
/// without a `data` array is refused, never read as empty.
fn every_page(
    mut get: impl FnMut(&str) -> Result<serde_json::Value, CliError>,
) -> Result<Vec<serde_json::Value>, CliError> {
    let mut vehicles = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(c) => format!("/vehicles?limit=100&cursor={c}"),
            None => "/vehicles?limit=100".to_owned(),
        };
        let page = get(&path)?;
        let Some(data) = page["data"].as_array() else {
            return Err(CliError::new(
                "vehicle_list_malformed",
                "GET /api/v1/vehicles returned a page without a `data` array",
            ));
        };
        vehicles.extend(data.iter().cloned());
        match page["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => return Ok(vehicles),
        }
    }
}

fn parse_class(name: &str) -> Result<VehicleClass, CliError> {
    VehicleClass::parse(name).ok_or_else(|| {
        CliError::new(
            "usage",
            format!("--class {name:?}: one of car, van, truck, bus"),
        )
        .with_path("--class")
    })
}

fn validate(args: ValidateArgs) -> CmdResult {
    let class = parse_class(&args.class)?;
    let dims = args.dims.as_deref().map(parse_dims).transpose()?;
    let glb = Glb::read(&args.model).map_err(|reason| {
        CliError::findings(
            "vehicle_glb_unreadable",
            format!("{}: {reason}", args.model.display()),
        )
        .with_path(args.model.display().to_string())
    })?;
    let report = vehicle::validate(&glb, Expect { class, dims });
    let value = json!({ "model": args.model, "report": report });
    Ok(if report.ok {
        Outcome::ok(value)
    } else {
        Outcome::findings(value)
    })
}

fn parse_dims(text: &str) -> Result<[f64; 3], CliError> {
    let parts: Vec<Option<f64>> = text
        .split(',')
        .map(|p| {
            p.trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
        })
        .collect();
    match parts.as_slice() {
        [Some(l), Some(w), Some(h)] => Ok([*l, *w, *h]),
        _ => Err(CliError::new(
            "usage",
            format!("--dims {text:?}: three positive metres, `length,width,height`"),
        )
        .with_path("--dims")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_uploads_the_model_to_its_grant_then_stores_it_by_upload() {
        let mut posted = Vec::new();
        let mut put_to = None;
        let stored = upload_and_store(
            b"glTF-bytes",
            &"a".repeat(64),
            json!({ "id": "delivery-van", "vehicleClass": "van", "license": "CC-BY-4.0", "attribution": "QA" }),
            |path, body| {
                posted.push((path.to_owned(), body.clone()));
                Ok(if path == "/uploads" {
                    json!({ "uploadId": "upl_1", "uploadUrl": "https://s3/put", "uploadHeaders": { "x-amz-checksum-sha256": "q" } })
                } else {
                    json!({ "catalogId": "workspace.delivery-van" })
                })
            },
            |url, body, headers| {
                put_to = Some((url.to_owned(), body.len(), headers.clone()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(stored["catalogId"], "workspace.delivery-van");
        assert_eq!(posted[0].0, "/uploads");
        assert_eq!(
            posted[0].1,
            json!({ "kind": "vehicle-model", "sha256": "a".repeat(64), "sizeBytes": 10 })
        );
        let (url, len, headers) = put_to.unwrap();
        assert_eq!((url.as_str(), len), ("https://s3/put", 10));
        assert_eq!(headers["x-amz-checksum-sha256"], "q");
        assert_eq!(posted[1].0, "/vehicles");
        assert_eq!(posted[1].1["uploadId"], "upl_1");
        assert_eq!(posted[1].1["id"], "delivery-van");

        // A grant without headers is refused before anything is uploaded.
        let refused = upload_and_store(
            b"x",
            "b",
            json!({}),
            |_, _| Ok(json!({ "uploadId": "upl_2", "uploadUrl": "u" })),
            |_, _, _| panic!("nothing is uploaded"),
        )
        .unwrap_err();
        assert_eq!(refused.to_json()["code"], "invalid_api_response");
    }

    #[test]
    fn every_page_follows_the_cursor_and_refuses_a_malformed_page() {
        let mut asked = Vec::new();
        let vehicles = every_page(|path| {
            asked.push(path.to_owned());
            Ok(if path.contains("cursor=page-2") {
                json!({ "data": [{ "catalogId": "workspace.delivery-van" }], "nextCursor": null })
            } else {
                json!({ "data": [{ "catalogId": "vehicle.sedan" }], "nextCursor": "page-2" })
            })
        })
        .unwrap();
        let ids: Vec<_> = vehicles
            .iter()
            .map(|v| v["catalogId"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["vehicle.sedan", "workspace.delivery-van"]);
        assert_eq!(
            asked,
            ["/vehicles?limit=100", "/vehicles?limit=100&cursor=page-2"]
        );

        let refused = every_page(|_| Ok(json!({ "items": [] }))).unwrap_err();
        assert_eq!(refused.to_json()["code"], "vehicle_list_malformed");
    }
}
