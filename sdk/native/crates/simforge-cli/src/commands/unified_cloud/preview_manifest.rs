use super::{cloud, Client};
use crate::contract::CliError;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub fn sidecar(out: &Path, suffix: &str) -> PathBuf {
    let mut name = out.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn download_json(
    c: &Client,
    artifacts: &[Value],
    name: &str,
    to: &Path,
) -> Result<Value, CliError> {
    let artifact = artifacts
        .iter()
        .find(|artifact| artifact["name"] == name)
        .ok_or_else(|| {
            CliError::new(
                "preview_identity_missing",
                format!("cloud preview returned no {name}"),
            )
        })?;
    cloud::download_artifact(c, artifact, to)?;
    serde_json::from_slice(
        &fs::read(to).map_err(|error| CliError::new("file_unreadable", error.to_string()))?,
    )
    .map_err(|error| CliError::new("invalid_api_response", error.to_string()))
}

pub fn publish(
    c: &Client,
    artifacts: &[Value],
    out: &Path,
    artifact: &Value,
    source: &Value,
    mode: &str,
    kind: &str,
    run_id: &str,
) -> Result<PathBuf, CliError> {
    let name = artifact["name"]
        .as_str()
        .ok_or_else(|| CliError::new("invalid_api_response", "preview artifact name missing"))?;
    let prefix = name
        .split('/')
        .next()
        .ok_or_else(|| CliError::new("invalid_api_response", "preview artifact prefix missing"))?;
    let native_manifest_name = format!("{name}.preview.json");
    let report_name = if artifacts
        .iter()
        .any(|artifact| artifact["name"] == format!("{name}.render.json"))
    {
        format!("{name}.render.json")
    } else {
        format!("{prefix}/render.json")
    };
    let report_path = sidecar(out, ".render.json");
    let report = download_json(c, artifacts, &report_name, &report_path)?;
    let manifest_path = sidecar(out, ".preview.json");
    let mut manifest = if artifacts
        .iter()
        .any(|artifact| artifact["name"] == native_manifest_name)
    {
        download_json(
            c,
            artifacts,
            &native_manifest_name,
            &sidecar(out, ".native-preview.json"),
        )?
    } else {
        let mut pins = Map::new();
        for key in [
            "input",
            "workspace",
            "map",
            "timeline",
            "actorAssets",
            "sky",
            "renderConfig",
            "lowering",
        ] {
            if let Some(value) = report.get(key) {
                pins.insert(key.to_owned(), value.clone());
            }
        }
        json!({"schema":"simforge.preview-manifest/v1","pins":pins})
    };
    if manifest["schema"] != "simforge.preview-manifest/v1" {
        return Err(CliError::new(
            "invalid_api_response",
            "cloud native preview manifest schema mismatch",
        ));
    }
    let runtime_name = format!("{prefix}/runtime-info.json");
    let runtime_path = sidecar(out, ".runtime.json");
    let mut runtime = download_json(c, artifacts, &runtime_name, &runtime_path)?;
    let expected_contract = format!(
        "{:x}",
        Sha256::digest(include_bytes!(
            "../../../../../../contracts/runtime-protocol.v1.json"
        ))
    );
    if runtime["runtimeProtocol"] != 1
        || runtime["rendererProtocol"] != 5
        || !runtime["engineSemver"].is_string()
        || runtime["contractSha256"] != expected_contract
    {
        return Err(CliError::new(
            "protocol_mismatch",
            "cloud preview runtime identity is incompatible",
        ));
    }
    let (sha, bytes) = crate::commands::fetch::digest_file(out)?;
    if artifact["sha256"] != sha || artifact["sizeBytes"].as_u64() != Some(bytes) {
        return Err(CliError::new(
            "artifact_checksum_mismatch",
            "downloaded preview differs from its authenticated artifact identity",
        ));
    }
    manifest["artifact"] = json!({"path":out,"sha256":sha,"bytes":bytes});
    manifest["renderReport"] = json!({"path":report_path,"sha256":crate::commands::fetch::digest_file(&report_path)?.0,"scope":"historical native report; intermediate worker paths are not published artifacts"});
    let binary_sha = runtime
        .as_object_mut()
        .and_then(|object| object.remove("binarySha256"))
        .ok_or_else(|| {
            CliError::new(
                "preview_identity_missing",
                "cloud runtime binary digest missing",
            )
        })?;
    manifest["runtimeBinarySha256"] = binary_sha;
    manifest["runtime"] = runtime;
    manifest["source"] = source.clone();
    manifest["preview"] = json!({"out":out,"mode":mode,"kind":kind});
    manifest["cloud"] = json!({"runId":run_id,"artifact":name});
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| CliError::new("encode_error", error.to_string()))?;
    let temporary = sidecar(out, ".preview.json.part");
    fs::write(&temporary, bytes)
        .and_then(|_| fs::rename(&temporary, &manifest_path))
        .map_err(|error| CliError::new("write_failed", error.to_string()))?;
    let _ = fs::remove_file(sidecar(out, ".native-preview.json"));
    Ok(manifest_path)
}
