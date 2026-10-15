//! Workspace-bound draft authoring through the shared operation catalogue.

use super::cloud::{seg, Client};
use crate::authoring_ops::{self, Operations};
use crate::contract::{CliError, CmdResult, Ctx, Outcome};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Map, Value};

#[derive(Debug, Subcommand)]
pub enum DraftsCommand {
    /// Create a workspace draft on an exact map version; prints its receipt (documentId, draftVersion, contentDigest, mapPin).
    Create(CreateArgs),
    /// The draft's current receipt and content: version, digest, map/actor/recording pins.
    Show(DraftArg),
    /// Apply catalogue operations in one atomic transaction against an explicit draft version and digest.
    #[command(
        long_about = "Apply catalogue operations in one atomic transaction against an explicit draft version and digest.\n\nEach operation is a subcommand with typed flags, chained with `--`: `drafts apply D --expected-version 3 --expected-digest SHA add-actor --id ego --catalog-id vehicle.sedan --lane-rsl R --s 40 -- set-weather --weather light_rain`. `simforge authoring operations` reports whether this credential, the host's feature flags and its workers allow each operation. A stale version or digest is refused, never retried: read the draft again with `drafts show`."
    )]
    Apply(ApplyArgs),
    /// Save the draft through the workspace store at an explicit version and digest.
    Save(CasArgs),
    /// Freeze the draft as an immutable revision (revisionId, revisionContentDigest) for simulate/render.
    Freeze(FreezeArgs),
    /// Validate every actor of the draft, optionally with the authoritative preview; exit 2 on findings.
    Validate(ValidateArgs),
}

#[derive(Debug, Subcommand)]
pub enum AuthoringCommand {
    /// The host's operation catalogue (simforge.authoring-operations/v1).
    Catalogue,
    /// What this credential may do on this host: operation, host-operation, feature and worker status.
    Capabilities,
    /// Every operation with its `drafts apply` flags, result schema and the host's status for this credential.
    Operations(OperationsArgs),
    /// Bounded map discovery without the map package: locations, lanes, movements or the actor catalogue.
    Discover(DiscoverArgs),
    /// Whether an actor can be placed at a lane position or scene point, optionally against a draft's actors.
    Spawn(SpawnArgs),
}

#[derive(Debug, Args)]
pub struct DraftArg {
    #[arg(value_name = "DRAFT_ID")]
    pub draft_id: String,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// The map version id the draft is pinned to.
    #[arg(long, value_name = "MAP_VERSION_ID")]
    pub map: String,
    #[arg(long)]
    pub name: String,
    /// The writable dataset; absent, the credential's default dataset (named in the receipt).
    #[arg(long, value_name = "DATASET_ID")]
    pub dataset: Option<String>,
    #[arg(long, value_name = "S")]
    pub clip_seconds: Option<f64>,
}

#[derive(Debug, Args)]
pub struct Cas {
    /// The draftVersion of the receipt this change is based on.
    #[arg(long, value_name = "N")]
    pub expected_version: u64,
    /// The contentDigest of the receipt this change is based on.
    #[arg(long, value_name = "SHA256")]
    pub expected_digest: String,
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    #[arg(value_name = "DRAFT_ID")]
    pub draft_id: String,
    #[command(flatten)]
    pub cas: Cas,
    #[command(subcommand)]
    pub operations: Operations,
}

#[derive(Debug, Args)]
pub struct CasArgs {
    #[arg(value_name = "DRAFT_ID")]
    pub draft_id: String,
    #[command(flatten)]
    pub cas: Cas,
}

#[derive(Debug, Args)]
pub struct FreezeArgs {
    #[arg(value_name = "DRAFT_ID")]
    pub draft_id: String,
    #[command(flatten)]
    pub cas: Cas,
    /// The revision's name.
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    #[arg(value_name = "DRAFT_ID")]
    pub draft_id: String,
    /// Warnings are findings too.
    #[arg(long)]
    pub strict: bool,
    /// Run the authoritative materializer/engine preview (every actor, collisions, quality issues).
    #[arg(long)]
    pub preview: bool,
}

#[derive(Debug, Args)]
pub struct OperationsArgs {
    /// One operation id (camelCase or its kebab-case command); absent, every operation.
    #[arg(value_name = "OP_ID")]
    pub operation: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DiscoverKind {
    Locations,
    Lanes,
    Movements,
    Catalogue,
}

impl DiscoverKind {
    pub(crate) fn s(self) -> &'static str {
        match self {
            Self::Locations => "locations",
            Self::Lanes => "lanes",
            Self::Movements => "movements",
            Self::Catalogue => "catalogue",
        }
    }
}

#[derive(Debug, Args)]
pub struct DiscoverArgs {
    #[arg(long, value_name = "MAP_VERSION_ID")]
    pub map: String,
    #[arg(long, value_enum)]
    pub kind: DiscoverKind,
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,
}

#[derive(Debug, Args)]
pub struct SpawnArgs {
    #[arg(long, value_name = "MAP_VERSION_ID")]
    pub map: String,
    #[arg(long, value_name = "ID")]
    pub catalog_id: String,
    /// A lane handle from `authoring discover --kind lanes` (with --s).
    #[arg(long, value_name = "RSL", requires = "s", conflicts_with_all = ["x", "z"], required_unless_present = "x")]
    pub lane_rsl: Option<String>,
    #[arg(long, requires = "lane_rsl", allow_hyphen_values = true)]
    pub s: Option<f64>,
    /// A scene point (with --z).
    #[arg(long, requires = "z", allow_hyphen_values = true)]
    pub x: Option<f64>,
    #[arg(long, requires = "x", allow_hyphen_values = true)]
    pub z: Option<f64>,
    #[arg(long, allow_hyphen_values = true)]
    pub heading_rad: Option<f64>,
    #[arg(long, allow_hyphen_values = true)]
    pub lateral_m: Option<f64>,
    #[arg(long)]
    pub opposing: bool,
    #[arg(long = "static")]
    pub r#static: bool,
    /// Check overlap against this draft's actors (needs --expected-version and --expected-digest).
    #[arg(long, value_name = "DRAFT_ID", requires_all = ["expected_version", "expected_digest"])]
    pub draft: Option<String>,
    #[arg(long, value_name = "N", requires = "draft")]
    pub expected_version: Option<u64>,
    #[arg(long, value_name = "SHA256", requires = "draft")]
    pub expected_digest: Option<String>,
}

pub(crate) fn draft_path(id: &str, action: Option<&str>) -> String {
    match action {
        Some(a) => format!("/drafts/{}/{a}", seg(id)),
        None => format!("/drafts/{}", seg(id)),
    }
}

pub(crate) fn map_path(map_version_id: &str, action: &str) -> String {
    format!("/authoring/maps/{}/{action}", seg(map_version_id))
}

pub(crate) fn discover_path(map: &str, kind: &str, limit: Option<u64>) -> String {
    match limit {
        Some(n) => format!(
            "{}?kind={}&limit={n}",
            map_path(map, "discovery"),
            seg(kind)
        ),
        None => format!("{}?kind={}", map_path(map, "discovery"), seg(kind)),
    }
}

pub(crate) fn capabilities(c: &Client) -> Result<Value, CliError> {
    c.get("/authoring/capabilities")
}

/// The host's catalogue: the bundled one's schema id with HTTP result schemas and root `$defs`.
pub(crate) fn host_catalogue(c: &Client) -> Result<Value, CliError> {
    let cat = c.get("/authoring/catalogue")?;
    let schema = &authoring_ops::catalogue()["schema"];
    if cat.get("schema") != Some(schema) || !cat["operations"].is_array() {
        return Err(CliError::new(
            "invalid_api_response",
            format!("the host's authoring catalogue is not {schema}"),
        )
        .with_detail(json!({"schema": cat.get("schema")})));
    }
    Ok(cat)
}

/// The host's status of an operation (`operations`) or host operation (`hostOperations`); `unreported` when absent.
pub(crate) fn status(caps: &Value, group: &str, id: &str) -> (String, Option<Value>) {
    let entry = caps.get(group).and_then(|g| g.get(id));
    let status = entry
        .and_then(|e| e.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unreported");
    (
        status.to_owned(),
        entry.and_then(|e| e.get("reason")).cloned(),
    )
}

/// Refuse locally what this credential, the host's flags or its workers do not allow.
pub(crate) fn require_available(caps: &Value, group: &str, ids: &[&str]) -> Result<(), CliError> {
    for id in ids {
        let (status, reason) = status(caps, group, id);
        if status != "available" {
            return Err(CliError::findings(
                "operation_unavailable",
                format!("the host reports {id} as {status}"),
            )
            .with_path(id.to_string())
            .with_detail(json!({"status": status, "reason": reason})));
        }
    }
    Ok(())
}

fn post_or_preview(c: &Client, ctx: &Ctx, path: String, body: Value) -> Result<Value, CliError> {
    if ctx.dry_run {
        return Ok(
            json!({"dryRun": true, "request": {"method": "POST", "path": path, "body": body}}),
        );
    }
    c.post(&path, &body)
}

fn cas_body(version: u64, digest: &str) -> Value {
    json!({"expectedVersion": version, "expectedDigest": digest})
}

/// One atomic transaction, refused before sending when the host reports an operation unavailable.
pub(crate) fn apply(
    c: &Client,
    ctx: &Ctx,
    draft: &str,
    version: u64,
    digest: &str,
    operations: Vec<Value>,
) -> Result<Value, CliError> {
    let caps = capabilities(c)?;
    require_available(&caps, "hostOperations", &["apply"])?;
    let ids: Vec<&str> = operations
        .iter()
        .filter_map(|o| o["type"].as_str())
        .collect();
    require_available(&caps, "operations", &ids)?;
    let mut body = cas_body(version, digest);
    body["operations"] = Value::Array(operations);
    post_or_preview(c, ctx, draft_path(draft, Some("transactions")), body)
}

pub(crate) fn create_body(
    map: &str,
    name: &str,
    dataset: Option<&str>,
    clip_seconds: Option<f64>,
) -> Value {
    let mut body = json!({"mapId": map, "name": name});
    if let Some(d) = dataset {
        body["datasetId"] = json!(d);
    }
    if let Some(s) = clip_seconds {
        body["clipSeconds"] = json!(s);
    }
    body
}

/// Findings (exit 2) when the host reports the draft not ok, or an error (a warning when strict).
pub(crate) fn validate_outcome(v: Value, strict: bool) -> Outcome {
    let severities: Vec<&str> = v["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d["severity"].as_str())
        .collect();
    let failing = v.get("ok") == Some(&json!(false))
        || severities.contains(&"error")
        || (strict && severities.contains(&"warning"));
    if failing {
        Outcome::findings(v)
    } else {
        Outcome::ok(v)
    }
}

pub fn drafts(c: &Client, x: DraftsCommand, ctx: &Ctx) -> CmdResult {
    let v = match x {
        DraftsCommand::Create(a) => post_or_preview(
            c,
            ctx,
            "/drafts".to_owned(),
            create_body(&a.map, &a.name, a.dataset.as_deref(), a.clip_seconds),
        )?,
        DraftsCommand::Show(a) => c.get(&draft_path(&a.draft_id, None))?,
        DraftsCommand::Apply(a) => apply(
            c,
            ctx,
            &a.draft_id,
            a.cas.expected_version,
            &a.cas.expected_digest,
            a.operations.0,
        )?,
        DraftsCommand::Save(a) => post_or_preview(
            c,
            ctx,
            draft_path(&a.draft_id, Some("save")),
            cas_body(a.cas.expected_version, &a.cas.expected_digest),
        )?,
        DraftsCommand::Freeze(a) => {
            let mut body = cas_body(a.cas.expected_version, &a.cas.expected_digest);
            if let Some(n) = a.name {
                body["name"] = json!(n);
            }
            post_or_preview(
                c,
                ctx,
                draft_path(&a.draft_id, Some("freeze-revision")),
                body,
            )?
        }
        DraftsCommand::Validate(a) => {
            let v = c.post(
                &draft_path(&a.draft_id, Some("validate")),
                &json!({"strict": a.strict, "preview": a.preview}),
            )?;
            return Ok(validate_outcome(v, a.strict));
        }
    };
    Ok(Outcome::ok(v))
}

/// The bundled operations (flags, input schemas) with the host's HTTP result
/// schema and its status for this credential merged in.
pub(crate) fn operations_doc(
    host: &Value,
    caps: &Value,
    only: Option<&str>,
) -> Result<Value, CliError> {
    let entries: Vec<Value> = match only {
        Some(id) => vec![authoring_ops::show_value(id)?],
        None => authoring_ops::catalogue()["operations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|op| op["id"].as_str())
            .map(authoring_ops::show_value)
            .collect::<Result<_, _>>()?,
    };
    let ops: Vec<Value> = entries
        .into_iter()
        .map(|mut op| {
            let id = op["id"].as_str().unwrap_or_default().to_owned();
            let (status, reason) = status(caps, "operations", &id);
            op["status"] = json!(status);
            op["reason"] = reason.unwrap_or(Value::Null);
            let served = host["operations"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|h| h["id"] == id.as_str());
            op["resultSchema"] = served
                .map(|h| h["resultSchema"].clone())
                .unwrap_or(Value::Null);
            op
        })
        .collect();
    Ok(json!({
        "schema": host["schema"],
        "$defs": host.get("$defs"),
        "operations": ops,
        "hostOperations": caps.get("hostOperations"),
        "credential": {"workspaceId": caps.get("workspaceId"), "role": caps.get("role"), "scopes": caps.get("scopes")},
        "features": caps.get("features"),
        "workers": caps.get("workers"),
    }))
}

pub(crate) fn spawn_body(a: &SpawnArgs) -> Value {
    let mut body = Map::new();
    body.insert("catalogId".into(), json!(a.catalog_id));
    let optional = [
        ("laneRsl", a.lane_rsl.clone().map(Value::from)),
        ("s", a.s.map(Value::from)),
        ("x", a.x.map(Value::from)),
        ("z", a.z.map(Value::from)),
        ("headingRad", a.heading_rad.map(Value::from)),
        ("lateralM", a.lateral_m.map(Value::from)),
        ("draftId", a.draft.clone().map(Value::from)),
        ("expectedVersion", a.expected_version.map(Value::from)),
        ("expectedDigest", a.expected_digest.clone().map(Value::from)),
        ("opposing", a.opposing.then_some(Value::Bool(true))),
        ("static", a.r#static.then_some(Value::Bool(true))),
    ];
    for (k, v) in optional {
        if let Some(v) = v {
            body.insert(k.into(), v);
        }
    }
    Value::Object(body)
}

pub fn authoring(c: &Client, x: AuthoringCommand) -> CmdResult {
    let v = match x {
        AuthoringCommand::Catalogue => host_catalogue(c)?,
        AuthoringCommand::Capabilities => capabilities(c)?,
        AuthoringCommand::Operations(a) => operations_doc(
            &host_catalogue(c)?,
            &capabilities(c)?,
            a.operation.as_deref(),
        )?,
        AuthoringCommand::Discover(a) => {
            c.get(&discover_path(&a.map, a.kind.s(), a.limit.map(u64::from)))?
        }
        AuthoringCommand::Spawn(a) => {
            let result = c.post(&map_path(&a.map, "spawn"), &spawn_body(&a))?;
            return Ok(if result.get("feasible") == Some(&json!(false)) {
                Outcome::findings(result)
            } else {
                Outcome::ok(result)
            });
        }
    };
    Ok(Outcome::ok(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_operation_the_host_does_not_report_available_is_refused() {
        let caps = json!({"operations": {"setWeather": {"status": "available"}, "setClock": {"status": "unauthorized", "reason": "role viewer"}}});
        assert!(require_available(&caps, "operations", &["setWeather"]).is_ok());
        let e = require_available(&caps, "operations", &["setWeather", "setClock"]).unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.path.as_deref()),
            ("operation_unavailable", Some("setClock"))
        );
        assert_eq!(e.detail.unwrap()["reason"], "role viewer");
        let e = require_available(&caps, "operations", &["addActor"]).unwrap_err();
        assert_eq!(e.detail.unwrap()["status"], "unreported");
    }

    #[test]
    fn validation_findings_follow_the_requested_strictness() {
        let warned = json!({"ok": true, "diagnostics": [{"severity": "warning", "code": "w"}]});
        assert_eq!(
            validate_outcome(warned.clone(), false).exit,
            crate::contract::Exit::Ok
        );
        assert_eq!(
            validate_outcome(warned, true).exit,
            crate::contract::Exit::Findings
        );
        assert_eq!(
            validate_outcome(json!({"ok": false, "diagnostics": []}), false).exit,
            crate::contract::Exit::Findings
        );
    }

    #[test]
    fn ids_are_escaped_as_one_path_segment() {
        assert_eq!(
            draft_path("../runs/x", Some("save")),
            "/drafts/..%2Fruns%2Fx/save"
        );
        assert_eq!(
            discover_path("map v1/2", "lanes", Some(5)),
            "/authoring/maps/map%20v1%2F2/discovery?kind=lanes&limit=5"
        );
    }
}
