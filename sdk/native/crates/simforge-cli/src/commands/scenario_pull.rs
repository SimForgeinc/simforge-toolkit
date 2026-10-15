use crate::auth;
use crate::contract::{CliError, CmdResult, Ctx, Outcome};
use clap::{Args, Parser};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Args)]
struct PullArgs {
    /// The hosted scenario document id.
    id: String,
    /// Output file. Default: <ID>.scenario.json.
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,
    /// SimForge host. Default: SIMFORGE_HOST, then the host of the last login, then simforge.ai.
    #[arg(long)]
    host: Option<String>,
    /// Overwrite an existing output file.
    #[arg(long)]
    force: bool,
}
/// Download a hosted scenario document as a local scenario JSON file.
#[derive(Parser)]
struct Input {
    #[command(flatten)]
    args: PullArgs,
}

pub fn help(root: &clap::Command) -> serde_json::Value {
    use clap::CommandFactory;
    let mut command = Input::command()
        .name("pull")
        .bin_name("simforge scenario pull");
    command.build();
    crate::help::document(root, &command, &["scenario".into(), "pull".into()])
}

pub fn run(argv: &[String], _ctx: &Ctx) -> CmdResult {
    let args = Input::try_parse_from(
        std::iter::once("scenario pull".to_owned()).chain(argv.iter().cloned()),
    )
    .map_err(|error| crate::parse_error(&error, argv))?
    .args;
    let out = args
        .out
        .unwrap_or_else(|| PathBuf::from(format!("{}.scenario.json", args.id)));
    if out.exists() && !args.force {
        return Err(
            CliError::new("file_exists", format!("{} already exists", out.display()))
                .with_detail(json!({"hint":"Pass --force to overwrite this file."})),
        );
    }
    if args.id.is_empty()
        || !args
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(CliError::new(
            "bad_value",
            "scenario id must contain only letters, digits, underscores or hyphens",
        ));
    }
    let host = auth::resolve_host(args.host.as_deref())?;
    let credential = auth::credential(&host)?.ok_or_else(|| auth::not_logged_in(&host))?;
    let base = credential
        .metadata
        .simforge_scenario_documents
        .as_ref()
        .ok_or_else(|| {
            CliError::new(
                "scenario_pull_unsupported",
                "this host does not expose scenario documents",
            )
        })?;
    let url = format!("{}/{}", base.trim_end_matches('/'), args.id);
    let timeout = Duration::from_secs(60);
    let mut token = credential.access_token();
    let mut reply = auth::http::get(&url, Some(&token), timeout)
        .map_err(|error| CliError::new("host_unreachable", error))?;
    if reply.status == 401 {
        if let Some(fresh) = credential.refresh_after_rejection(&token)? {
            token = fresh;
            reply = auth::http::get(&url, Some(&token), timeout)
                .map_err(|error| CliError::new("host_unreachable", error))?;
        }
    }
    let answer = reply
        .json()
        .ok_or_else(|| CliError::new("invalid_api_response", "host response is not JSON"))?;
    match reply.status {
        200 => {}
        403 => return Err(CliError::new(
            "scenario_pull_forbidden",
            "this credential cannot read the scenario",
        )
        .with_detail(
            json!({"hint":"Run simforge login with scenarios:read for the intended workspace."}),
        )),
        404 => {
            return Err(CliError::new(
                "scenario_not_found",
                "scenario not found in this credential's workspace",
            ))
        }
        status => {
            return Err(CliError::new(
                "scenario_pull_failed",
                format!("host returned HTTP {status}"),
            ))
        }
    }
    let mut content = answer
        .get("content")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| {
            CliError::new("invalid_api_response", "host returned no scenario document")
        })?;
    let xodr = answer["mapXodrSha256"].as_str();
    let pinned = if let Some(sha) = xodr {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CliError::new(
                "invalid_api_response",
                "host returned an invalid map digest",
            ));
        }
        let source = content["sourceMap"]
            .as_object_mut()
            .ok_or_else(|| CliError::new("invalid_api_response", "scenario has no sourceMap"))?;
        if source.get("xodrSha256").is_some_and(|old| old != sha) {
            return Err(CliError::new(
                "map_pin_mismatch",
                "scenario digest differs from its hosted map",
            ));
        }
        source.insert("xodrSha256".into(), json!(sha));
        true
    } else {
        false
    };
    let map_id = content["sourceMap"]["mapId"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if map_id.is_empty()
        || map_id == "."
        || map_id == ".."
        || !map_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(CliError::new(
            "invalid_api_response",
            "scenario map id is not a safe map identifier",
        ));
    }
    let receipt = crate::paths::maps_root(None)?
        .value
        .join(".corpus")
        .join(&map_id)
        .join(".map-release.json");
    let installed = if receipt.is_file() {
        json!({"mapId":map_id,"installed":true,"verification":"installation-receipt","xodrSha256":xodr})
    } else {
        json!({"installed":false,"error":CliError::new("unknown_map","the scenario's map is not installed").to_json()})
    };
    let revision = super::unified_cloud::store::create(&out, &content, args.force)?;
    Ok(Outcome::ok(
        json!({"file":out,"host":host.name,"id":answer.get("id").unwrap_or(&json!(args.id)),"title":answer["title"],"draftVersion":answer["draftVersion"],"mapVersionId":answer["mapVersionId"],"mapSourceMapId":answer["mapSourceMapId"],"mapXodrSha256":xodr,"pinnedMapDigest":pinned,"map":installed,"version":revision.version,"digest":revision.digest}),
    ))
}
