use crate::contract::{CliError, CmdResult, Outcome};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

pub fn attach(outcome: Outcome, target: &Path) -> CmdResult {
    let manifest_path = outcome.value["manifest"].as_str().ok_or_else(|| {
        CliError::new(
            "preview_identity_missing",
            "runtime preview did not publish its artifact manifest",
        )
        .with_detail(json!({"hint":"Install the runtime release matching this CLI."}))
    })?;
    let path = Path::new(manifest_path);
    let bytes = fs::read(path)
        .map_err(|error| CliError::new("preview_identity_missing", error.to_string()))?;
    let mut manifest: Value = serde_json::from_slice(&bytes)
        .map_err(|error| CliError::new("invalid_runtime_response", error.to_string()))?;
    if manifest["schema"] != "simforge.preview-manifest/v1" {
        return Err(CliError::new(
            "invalid_runtime_response",
            "preview manifest schema mismatch",
        ));
    }
    let artifact_path = manifest["artifact"]["path"].as_str().ok_or_else(|| {
        CliError::new("preview_identity_missing", "preview artifact path missing")
    })?;
    let (sha, count) = super::fetch::digest_file(Path::new(artifact_path))?;
    if manifest["artifact"]["sha256"] != sha
        || manifest["artifact"]["bytes"].as_u64() != Some(count)
    {
        return Err(CliError::new(
            "artifact_checksum_mismatch",
            "local preview does not match its published manifest",
        ));
    }
    if target.is_file() {
        let (sha, count) = super::fetch::digest_file(target)?;
        let mut source = json!({"file":target,"sha256":sha,"bytes":count});
        let bytes = fs::read(target)
            .map_err(|error| CliError::new("file_unreadable", error.to_string()))?;
        if let Ok(document) = serde_json::from_slice::<Value>(&bytes) {
            source["canonicalDigest"] = json!(super::unified_cloud::store::digest(&document)?);
        }
        manifest["source"] = source;
    } else {
        manifest["source"] = json!({"workspace":target});
    }
    let temporary = path.with_extension("json.part");
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| CliError::new("encode_error", error.to_string()))?;
    fs::write(&temporary, bytes)
        .and_then(|_| fs::rename(&temporary, path))
        .map_err(|error| CliError::new("write_failed", error.to_string()))?;
    Ok(outcome)
}
