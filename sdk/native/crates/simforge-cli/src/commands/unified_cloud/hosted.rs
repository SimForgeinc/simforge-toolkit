//! A scenario file's hosted revision: imported and frozen once per content, then reused.
//!
//! The first `--cloud` command on a file (a run or a `--dry-run` price) imports it as a draft and
//! freezes it as an immutable revision in the workspace (free authoring), and records that revision
//! beside the file in `.<file>.cloud-revisions.json` (`simforge.cloud-revisions/v1`), keyed by the
//! host, the workspace, the file's canonical digest and the map pin it was imported with. A later
//! command on the same content reuses it instead of importing again, so pricing first and running
//! next runs exactly the revision that was priced. The authoring sidecar (the runtime's store) is
//! not touched. A recorded revision whose scenario the host no longer has is replaced, and the
//! replacement is named in the output.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{freeze, import, map_pin, store, str_at, Revision};
use crate::commands::cloud::{seg, Client};
use crate::contract::{CliError, Ctx};

const SCHEMA: &str = "simforge.cloud-revisions/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    host: String,
    workspace_id: String,
    digest: String,
    pin: Value,
    scenario_id: String,
    revision_id: String,
    map_pin: Value,
    revision_content_digest: Value,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema: String,
    revisions: Vec<Entry>,
}

fn record_path(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    file.with_file_name(format!(".{name}.cloud-revisions.json"))
}

fn read_record(path: &Path) -> Result<Record, CliError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Record::default()),
        Err(e) => {
            return Err(CliError::new(
                "file_unreadable",
                format!("{}: {e}", path.display()),
            ))
        }
    };
    let record: Record = serde_json::from_slice(&bytes).map_err(|e| {
        CliError::new("cloud_revisions_invalid", format!("{}: {e}", path.display()))
            .with_path(path.display().to_string())
            .with_detail(json!({ "hint": "delete the file; the next --cloud command imports the scenario again" }))
    })?;
    if record.schema != SCHEMA {
        return Err(CliError::new(
            "cloud_revisions_invalid",
            format!("{} is not {SCHEMA}", path.display()),
        )
        .with_path(path.display().to_string()));
    }
    Ok(record)
}

/// Replace the record atomically (write beside it, then rename).
fn write_record(path: &Path, record: &Record) -> Result<(), CliError> {
    let staged = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|e| CliError::new("encode_error", e.to_string()))?;
    fs::write(&staged, bytes)
        .and_then(|()| fs::rename(&staged, path))
        .map_err(|e| CliError::new("write_failed", format!("{}: {e}", path.display())))
}

fn matches(entry: &Entry, host: &str, workspace: &str, digest: &str, pin: &Value) -> bool {
    entry.host == host
        && entry.workspace_id == workspace
        && entry.digest == digest
        && entry.pin == *pin
}

/// The immutable hosted revision of `file`'s current content: the recorded one, else a new import.
pub(super) fn file_revision(c: &Client, file: &Path, ctx: &Ctx) -> Result<Revision, CliError> {
    let (document, digest, version) = {
        let locked = store::open(file)?;
        let version = locked
            .recorded
            .as_ref()
            .filter(|r| r.digest == locked.digest)
            .map(|r| r.version);
        (locked.document, locked.digest, version)
    };
    let explicit = ctx.map_version.as_deref();
    let pin = map_pin::request(&document, explicit)?;
    let host = c.origin().to_owned();
    let workspace = c.acting_workspace()?;
    let path = record_path(file);
    let mut record = read_record(&path)?;
    let mut replaced = Value::Null;
    if let Some(at) = record
        .revisions
        .iter()
        .position(|e| matches(e, &host, &workspace, &digest, &pin))
    {
        let entry = record.revisions[at].clone();
        match c.get(&format!("/scenarios/{}", seg(&entry.scenario_id))) {
            Ok(_) => {
                return Ok(revision(
                    file,
                    &digest,
                    version,
                    entry,
                    "reused",
                    Value::Null,
                ))
            }
            Err(e) if e.code == "not_found" => {
                replaced = json!({ "scenarioId": entry.scenario_id, "revisionId": entry.revision_id, "reason": "the host no longer has this scenario" });
                record.revisions.remove(at);
            }
            Err(e) => return Err(e),
        }
    }
    let receipt = import(c, file, &document, &digest, false, explicit)?;
    let frozen = freeze(c, &receipt)?;
    let entry = Entry {
        host,
        workspace_id: workspace,
        digest: digest.clone(),
        pin,
        scenario_id: str_at(&frozen, "/documentId")?.to_owned(),
        revision_id: str_at(&frozen, "/revisionId")?.to_owned(),
        map_pin: frozen["mapPin"].clone(),
        revision_content_digest: frozen["revisionContentDigest"].clone(),
    };
    record.schema = SCHEMA.to_owned();
    record.revisions.push(entry.clone());
    write_record(&path, &record)?;
    Ok(revision(file, &digest, version, entry, "saved", replaced))
}

fn revision(
    file: &Path,
    digest: &str,
    version: Option<u64>,
    entry: Entry,
    hosted: &str,
    replaced: Value,
) -> Revision {
    let ids = json!({ "scenarioId": entry.scenario_id, "revisionId": entry.revision_id });
    let mut source = json!({
        "file": file,
        "digest": digest,
        "version": version,
        "mapPin": entry.map_pin,
        "revisionContentDigest": entry.revision_content_digest,
        "hostedRevision": hosted,
    });
    if !replaced.is_null() {
        source["replaced"] = replaced;
    }
    Revision {
        created: if hosted == "saved" { ids } else { json!({}) },
        source,
        scenario_id: entry.scenario_id,
        revision_id: entry.revision_id,
    }
}

/// A dry run's answer with the revision it priced, and whether that revision was just saved.
pub(super) fn priced(mut answer: Value, rev: &Revision) -> Value {
    answer["source"] = rev.source.clone();
    answer["created"] = rev.created.clone();
    let note = match rev.source["hostedRevision"].as_str() {
        Some("saved") => format!("The scenario file was saved to the workspace as scenario {} revision {} (free); the run that follows reuses this revision.", rev.scenario_id, rev.revision_id),
        Some("reused") => format!("Priced on revision {} already saved for this file's content; the run that follows reuses it.", rev.revision_id),
        _ => return answer,
    };
    answer["note"] = json!(note);
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(digest: &str) -> Entry {
        Entry {
            host: "https://dev.simforge.ai".into(),
            workspace_id: "ws_1".into(),
            digest: digest.into(),
            pin: json!({}),
            scenario_id: "doc_1".into(),
            revision_id: "rev_1".into(),
            map_pin: json!({"mapId":"usmap_1"}),
            revision_content_digest: json!("abc"),
        }
    }

    #[test]
    fn a_recorded_revision_matches_only_its_host_workspace_content_and_pin() {
        let e = entry("d1");
        assert!(matches(
            &e,
            "https://dev.simforge.ai",
            "ws_1",
            "d1",
            &json!({})
        ));
        assert!(!matches(
            &e,
            "https://simforge.ai",
            "ws_1",
            "d1",
            &json!({})
        ));
        assert!(!matches(
            &e,
            "https://dev.simforge.ai",
            "ws_2",
            "d1",
            &json!({})
        ));
        assert!(!matches(
            &e,
            "https://dev.simforge.ai",
            "ws_1",
            "d2",
            &json!({})
        ));
        assert!(!matches(
            &e,
            "https://dev.simforge.ai",
            "ws_1",
            "d1",
            &json!({"mapId":"usmap_2"})
        ));
    }

    #[test]
    fn the_record_round_trips_beside_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("left-turn.json");
        let path = record_path(&file);
        assert_eq!(
            path,
            dir.path().join(".left-turn.json.cloud-revisions.json")
        );
        assert!(read_record(&path).unwrap().revisions.is_empty());
        let record = Record {
            schema: SCHEMA.into(),
            revisions: vec![entry("d1")],
        };
        write_record(&path, &record).unwrap();
        assert_eq!(read_record(&path).unwrap().revisions, vec![entry("d1")]);
        fs::write(&path, "{\"schema\":\"other\",\"revisions\":[]}").unwrap();
        assert_eq!(
            read_record(&path).unwrap_err().code,
            "cloud_revisions_invalid"
        );
    }

    #[test]
    fn a_dry_run_says_which_revision_it_priced() {
        let file = Path::new("s.json");
        let saved = revision(file, "d1", Some(2), entry("d1"), "saved", Value::Null);
        let answer = priced(json!({"dryRun":true,"costEstimateCents":34}), &saved);
        assert_eq!(answer["costEstimateCents"], 34);
        assert_eq!(answer["created"]["revisionId"], "rev_1");
        assert!(answer["note"]
            .as_str()
            .unwrap()
            .contains("saved to the workspace"));
        let reused = revision(file, "d1", Some(2), entry("d1"), "reused", Value::Null);
        let answer = priced(json!({"dryRun":true}), &reused);
        assert_eq!(answer["created"], json!({}));
        assert_eq!(answer["source"]["hostedRevision"], "reused");
    }
}
