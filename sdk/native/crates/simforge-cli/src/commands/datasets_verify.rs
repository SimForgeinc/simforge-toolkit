//! `simforge datasets verify <dir>`: check a downloaded `simforge.dataset-export/v1`
//! export (sdk/docs/engineering/dataset-export.md) without the network.
//!
//! Every file SHA256SUMS lists must be present with its sha256; SHA256SUMS
//! and scenarios.jsonl must match the digests the manifest records; every
//! scenarios.jsonl row's files must be listed; the row count must equal the
//! manifest's. The export id is the sha256 of manifest.json. With the local
//! build, every scenario package is also verified by the package reader.

use crate::contract::{CliError, CmdResult, Outcome};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use super::fetch::digest_file;

pub const SCHEMA: &str = "simforge.dataset-export/v1";

fn read(path: &Path) -> Result<Vec<u8>, CliError> {
    fs::read(path)
        .map_err(|e| CliError::findings("export_file_missing", format!("{}: {e}", path.display())))
}

fn finding(code: &str, reason: String) -> CliError {
    CliError::findings(code, reason)
}

/// A path from SHA256SUMS or a row: relative, `/`-separated, plain names.
fn relative(dir: &Path, path: &str) -> Result<PathBuf, CliError> {
    let plain =
        |part: &str| !part.is_empty() && part != "." && part != ".." && !part.contains('\\');
    if path.starts_with('/') || !path.split('/').all(plain) {
        return Err(finding(
            "export_path_invalid",
            format!("{path:?} is not a relative path"),
        ));
    }
    Ok(dir.join(path))
}

fn hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Verify the export at `dir`; `packages` also verifies every scenario.zip (local build).
pub fn verify(dir: &Path, packages: bool) -> CmdResult {
    let manifest_bytes = read(&dir.join("manifest.json"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| finding("export_manifest_invalid", format!("manifest.json: {e}")))?;
    if manifest["schema"] != SCHEMA {
        return Err(finding(
            "export_schema_unsupported",
            format!(
                "manifest.json is {}, this CLI reads {SCHEMA}",
                manifest["schema"]
            ),
        ));
    }
    let expect = |name: &str| -> Result<(String, u64), CliError> {
        let entry = &manifest["files"][name];
        match (entry["sha256"].as_str(), entry["size"].as_u64()) {
            (Some(sha), Some(size)) => Ok((sha.to_owned(), size)),
            _ => Err(finding(
                "export_manifest_invalid",
                format!("manifest.json names no digest for {name}"),
            )),
        }
    };
    let mut checked = 0u64;
    let mut bytes = 0u64;
    for name in ["SHA256SUMS", "scenarios.jsonl"] {
        let (sha, size) = expect(name)?;
        let (got, len) = digest_file(&dir.join(name))?;
        if got != sha || len != size {
            return Err(finding(
                "export_digest_mismatch",
                format!("{name}: manifest says {sha} ({size} bytes), found {got} ({len} bytes)"),
            ));
        }
    }
    let sums_text = String::from_utf8(read(&dir.join("SHA256SUMS"))?)
        .map_err(|_| finding("export_sums_invalid", "SHA256SUMS is not UTF-8".into()))?;
    let mut listed = BTreeMap::new();
    for (n, line) in sums_text.lines().enumerate() {
        let (sha, path) = line.split_once("  ").ok_or_else(|| {
            finding(
                "export_sums_invalid",
                format!("SHA256SUMS line {}: not `<sha256>  <path>`", n + 1),
            )
        })?;
        let file = relative(dir, path)?;
        let (got, len) = digest_file(&file).map_err(|_| {
            finding(
                "export_file_missing",
                format!("{path} is listed but missing"),
            )
        })?;
        if got != sha {
            return Err(finding(
                "export_digest_mismatch",
                format!("{path}: listed {sha}, found {got}"),
            ));
        }
        listed.insert(path.to_owned(), sha.to_owned());
        checked += 1;
        bytes += len;
    }
    let rows_text = String::from_utf8(read(&dir.join("scenarios.jsonl"))?)
        .map_err(|_| finding("export_rows_invalid", "scenarios.jsonl is not UTF-8".into()))?;
    let mut rows = 0u64;
    let mut verified_packages = 0u64;
    for (n, line) in rows_text.lines().enumerate() {
        let row: Value = serde_json::from_str(line).map_err(|e| {
            finding(
                "export_rows_invalid",
                format!("scenarios.jsonl line {}: {e}", n + 1),
            )
        })?;
        rows += 1;
        let files = row["members"].as_array().into_iter().flatten().chain(
            row["renders"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|r| r["files"].as_array().into_iter().flatten()),
        );
        for file in files {
            let path = file["path"].as_str().unwrap_or_default();
            if listed.get(path).map(String::as_str) != file["sha256"].as_str() {
                return Err(finding(
                    "export_row_unlisted",
                    format!(
                        "{} lists {path:?}, which SHA256SUMS does not list with that digest",
                        row["id"]
                    ),
                ));
            }
            if packages && file["role"] == "package" {
                verify_package(&relative(dir, path)?)?;
                verified_packages += 1;
            }
        }
    }
    if Some(rows) != manifest["counts"]["scenarios"].as_u64() {
        return Err(finding(
            "export_rows_count",
            format!(
                "scenarios.jsonl has {rows} rows, the manifest counts {}",
                manifest["counts"]["scenarios"]
            ),
        ));
    }
    Ok(Outcome::ok(json!({
        "exportId": hex(&manifest_bytes),
        "dir": dir,
        "scenarios": rows,
        "files": checked,
        "bytes": bytes,
        "packagesVerified": if packages { json!(verified_packages) } else { Value::Null },
        "verified": true,
    })))
}

fn verify_package(path: &Path) -> Result<(), CliError> {
    let result = crate::runtime::command(&[
        "package".into(),
        "verify".into(),
        path.display().to_string(),
    ])?;
    if result.exit == crate::contract::Exit::Ok {
        Ok(())
    } else {
        Err(CliError::findings(
            "package_invalid",
            "the runtime found invalid package content",
        )
        .with_detail(result.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(b: &[u8]) -> String {
        hex(b)
    }

    fn write_export(dir: &Path) {
        let pkg = b"package";
        fs::create_dir_all(dir.join("scenarios/a.000000000000")).unwrap();
        fs::write(dir.join("scenarios/a.000000000000/scenario.zip"), pkg).unwrap();
        let row = json!({"id": "doc_a", "members": [{"path": "scenarios/a.000000000000/scenario.zip", "sha256": sha(pkg), "size": 7, "role": "package"}], "renders": []});
        let rows = format!("{}\n", row);
        fs::write(dir.join("scenarios.jsonl"), &rows).unwrap();
        let sums = format!(
            "{}  scenarios.jsonl\n{}  scenarios/a.000000000000/scenario.zip\n",
            sha(rows.as_bytes()),
            sha(pkg)
        );
        fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
        let manifest = json!({
            "schema": SCHEMA,
            "counts": {"scenarios": 1},
            "files": {
                "SHA256SUMS": {"sha256": sha(sums.as_bytes()), "size": sums.len()},
                "scenarios.jsonl": {"sha256": sha(rows.as_bytes()), "size": rows.len()},
            },
        });
        fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
    }

    #[test]
    fn a_complete_export_verifies_and_a_changed_byte_is_named() {
        let dir = tempfile::tempdir().unwrap();
        write_export(dir.path());
        let out = verify(dir.path(), false).unwrap();
        assert_eq!(out.value["scenarios"], 1);
        assert_eq!(out.value["files"], 2);
        assert_eq!(
            out.value["exportId"],
            sha(&fs::read(dir.path().join("manifest.json")).unwrap())
        );
        fs::write(
            dir.path().join("scenarios/a.000000000000/scenario.zip"),
            b"pAckage",
        )
        .unwrap();
        assert_eq!(
            verify(dir.path(), false).unwrap_err().code,
            "export_digest_mismatch"
        );
        fs::remove_file(dir.path().join("scenarios/a.000000000000/scenario.zip")).unwrap();
        assert_eq!(
            verify(dir.path(), false).unwrap_err().code,
            "export_file_missing"
        );
    }

    #[test]
    fn a_path_that_escapes_the_export_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            relative(dir.path(), "../x").unwrap_err().code,
            "export_path_invalid"
        );
        assert_eq!(
            relative(dir.path(), "/etc/passwd").unwrap_err().code,
            "export_path_invalid"
        );
        assert!(relative(dir.path(), "scenarios/a/scenario.zip").is_ok());
    }
}
