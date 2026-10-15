use crate::contract::CliError;
use serde_json::{json, Value};
use std::fs;

/// The draft import's map pin: the installed native release (its digest) and/or the explicit
/// `--map-version`; empty when neither exists.
pub fn request(document: &Value, explicit: Option<&str>) -> Result<Value, CliError> {
    let map_id = document["sourceMap"]["mapId"]
        .as_str()
        .ok_or_else(|| CliError::new("map_pin_required", "scenario sourceMap.mapId missing"))?;
    let cache = crate::paths::maps_root(None)?.value.join(".corpus");
    let mut receipts = Vec::new();
    match fs::read_dir(&cache) {
        Ok(entries) => {
            for entry in entries {
                let entry =
                    entry.map_err(|e| CliError::new("map_receipt_unreadable", e.to_string()))?;
                if !entry
                    .file_type()
                    .map_err(|e| CliError::new("map_receipt_unreadable", e.to_string()))?
                    .is_dir()
                {
                    continue;
                }
                let path = entry.path().join(".map-release.json");
                if !path.is_file() {
                    continue;
                }
                let bytes = fs::read(&path)
                    .map_err(|e| CliError::new("map_receipt_unreadable", e.to_string()))?;
                let receipt: Value = serde_json::from_slice(&bytes).map_err(|e| {
                    CliError::new("map_receipt_invalid", e.to_string())
                        .with_path(path.display().to_string())
                })?;
                if receipt["name"] == map_id {
                    if receipt["schema"] != "simforge.map-installation.v1"
                        || receipt["profile"] != "native"
                    {
                        return Err(CliError::new(
                            "map_receipt_invalid",
                            "installed native map receipt schema invalid",
                        )
                        .with_path(path.display().to_string()));
                    }
                    let digest = receipt["releaseDigest"]
                        .as_str()
                        .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
                        .ok_or_else(|| {
                            CliError::new(
                                "map_receipt_invalid",
                                "installed map releaseDigest invalid",
                            )
                        })?;
                    receipts.push(digest.to_owned());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(CliError::new("map_receipt_unreadable", error.to_string())),
    }
    if receipts.len() > 1 {
        return Err(CliError::new(
            "map_pin_ambiguous",
            "multiple installed native receipts name this map",
        ));
    }
    let mut pin = json!({});
    if let Some(digest) = receipts.first() {
        pin["mapReleaseDigest"] = json!(digest);
    }
    if let Some(version) = explicit {
        pin["mapId"] = json!(version);
    }
    // Neither: the host pins the one readable version carrying the document's OpenDRIVE digest
    // (`map_pin_unavailable` / `map_pin_ambiguous` otherwise), so a cloud-only install needs no map download.
    Ok(pin)
}
