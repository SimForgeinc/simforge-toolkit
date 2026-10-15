//! The local scenario file's compare-and-swap store, engine-free and byte-compatible with the runtime's
//! `scenario` store: the same revision sidecar, lock, journal and simforge.canonical-json/v1 digest.

use std::cmp::Ordering;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::contract::CliError;

const SIDECAR_SCHEMA: &str = "simforge.authoring-revision/v1";
const JOURNAL_SCHEMA: &str = "simforge.authoring-journal/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub version: u64,
    pub digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Sidecar {
    schema: String,
    version: u64,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    schema: String,
    previous_revision: Option<Revision>,
    next: Revision,
}

/// simforge.canonical-json/v1: UTF-16-sorted keys, no whitespace, ECMAScript number text.
pub fn canonical_json(value: &Value) -> Result<String, CliError> {
    fn write(value: &Value, out: &mut String) -> Result<(), CliError> {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    out.push_str(&i.to_string());
                } else if let Some(u) = n.as_u64() {
                    out.push_str(&u.to_string());
                } else {
                    let f = n.as_f64().filter(|f| f.is_finite()).ok_or_else(|| {
                        CliError::new("invalid_data", format!("non-finite number {n}"))
                    })?;
                    if f == 0.0 {
                        out.push('0');
                    } else {
                        out.push_str(ryu_js::Buffer::new().format(f));
                    }
                }
            }
            Value::String(s) => out.push_str(&serde_json::to_string(s).expect("strings encode")),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out)?;
                }
                out.push(']');
            }
            Value::Object(map) => {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_by(|(a, _), (b, _)| utf16_order(a, b));
                out.push('{');
                for (i, (key, item)) in entries.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).expect("keys encode"));
                    out.push(':');
                    write(item, out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }
    let mut out = String::new();
    write(value, &mut out)?;
    Ok(out)
}

fn utf16_order(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.as_bytes().cmp(b.as_bytes());
    }
    a.encode_utf16().cmp(b.encode_utf16())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The document's content id: sha256 of its canonical JSON.
pub fn digest(document: &Value) -> Result<String, CliError> {
    Ok(sha256_hex(canonical_json(document)?.as_bytes()))
}

fn io_error(path: &Path, error: std::io::Error) -> CliError {
    CliError::new("io_error", format!("{}: {error}", path.display()))
        .with_path(path.display().to_string())
}

fn sibling(file: &Path, name: impl FnOnce(&str) -> String) -> PathBuf {
    let base = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    file.with_file_name(name(&base))
}

fn sidecar_path(file: &Path) -> PathBuf {
    sibling(file, |n| format!("{n}.authoring.json"))
}

fn hidden(file: &Path, suffix: &str) -> PathBuf {
    sibling(file, |n| format!(".{n}.authoring.{suffix}"))
}

fn lock(file: &Path) -> Result<File, CliError> {
    if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    }
    let path = hidden(file, "lock");
    let handle = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| io_error(&path, e))?;
    fs4::FileExt::lock(&handle).map_err(|e| io_error(&path, e))?;
    Ok(handle)
}

fn read_document(file: &Path) -> Result<Value, CliError> {
    let text = std::fs::read_to_string(file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CliError::new("file_not_found", format!("cannot read {}", file.display()))
                .with_path(file.display().to_string())
        } else {
            io_error(file, e)
        }
    })?;
    serde_json::from_str(&text).map_err(|e| {
        CliError::new("invalid_json", e.to_string()).with_path(file.display().to_string())
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    schema: &str,
    code: &str,
) -> Result<Option<T>, CliError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(path, e)),
    };
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if value["schema"] != schema {
        return Err(
            CliError::new(code, format!("{} is not a {schema} record", path.display()))
                .with_path(path.display().to_string()),
        );
    }
    serde_json::from_value(value).map(Some).map_err(|e| {
        CliError::new(code, format!("{}: {e}", path.display()))
            .with_path(path.display().to_string())
    })
}

fn read_sidecar(file: &Path) -> Result<Option<Revision>, CliError> {
    Ok(
        read_json::<Sidecar>(&sidecar_path(file), SIDECAR_SCHEMA, "revision_invalid")?.map(|s| {
            Revision {
                version: s.version,
                digest: s.digest,
            }
        }),
    )
}

fn sidecar_bytes(revision: &Revision) -> Vec<u8> {
    let sidecar = Sidecar {
        schema: SIDECAR_SCHEMA.into(),
        version: revision.version,
        digest: revision.digest.clone(),
    };
    serde_json::to_vec_pretty(&sidecar).expect("sidecar serializes")
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut out = File::create(path)?;
    out.write_all(bytes)?;
    out.sync_all()
}

fn sync_dir(file: &Path) -> std::io::Result<()> {
    match file.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => File::open(dir)?.sync_all(),
        None => File::open(".")?.sync_all(),
    }
}

fn remove_if_present(path: &Path) -> Result<(), CliError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_error(path, e)),
        _ => Ok(()),
    }
}

fn restore_sidecar(file: &Path, revision: Option<&Revision>) -> Result<(), CliError> {
    let path = sidecar_path(file);
    match revision {
        Some(revision) => {
            let staged = hidden(file, "restore");
            write_synced(&staged, &sidecar_bytes(revision)).map_err(|e| io_error(&staged, e))?;
            std::fs::rename(&staged, &path).map_err(|e| io_error(&path, e))
        }
        None => remove_if_present(&path),
    }
}

fn clear_staging(file: &Path) {
    for suffix in ["next", "next-revision", "journal.tmp", "restore"] {
        let _ = std::fs::remove_file(hidden(file, suffix));
    }
}

/// Finish or undo a commit a crash interrupted: the document decides which.
fn recover(file: &Path) -> Result<(), CliError> {
    let path = hidden(file, "journal");
    let Some(journal) = read_json::<Journal>(&path, JOURNAL_SCHEMA, "journal_invalid")? else {
        return Ok(());
    };
    let current = read_document(file).ok().map(|d| digest(&d)).transpose()?;
    if current.as_deref() == Some(journal.next.digest.as_str()) {
        restore_sidecar(file, Some(&journal.next))?;
    } else {
        restore_sidecar(file, journal.previous_revision.as_ref())?;
    }
    clear_staging(file);
    remove_if_present(&path)
}

/// Replace the document and its revision as one commit; the document rename is the commit point.
fn install(
    file: &Path,
    document: &Value,
    version: u64,
    previous: Option<&Revision>,
) -> Result<Revision, CliError> {
    let text = serde_json::to_string_pretty(document)
        .map(|text| format!("{text}\n"))
        .expect("JSON values serialize");
    let next = Revision {
        version,
        digest: digest(document)?,
    };
    let (staged_document, staged_sidecar) = (hidden(file, "next"), hidden(file, "next-revision"));
    let journal_path = hidden(file, "journal");
    let prepare = || -> Result<(), CliError> {
        write_synced(&staged_document, text.as_bytes())
            .map_err(|e| io_error(&staged_document, e))?;
        write_synced(&staged_sidecar, &sidecar_bytes(&next))
            .map_err(|e| io_error(&staged_sidecar, e))?;
        let journal = Journal {
            schema: JOURNAL_SCHEMA.into(),
            previous_revision: previous.cloned(),
            next: next.clone(),
        };
        let staged_journal = hidden(file, "journal.tmp");
        let bytes = serde_json::to_vec_pretty(&journal).expect("journal serializes");
        write_synced(&staged_journal, &bytes).map_err(|e| io_error(&staged_journal, e))?;
        std::fs::rename(&staged_journal, &journal_path).map_err(|e| io_error(&journal_path, e))?;
        sync_dir(file).map_err(|e| io_error(file, e))
    };
    let abandon = || {
        clear_staging(file);
        let _ = std::fs::remove_file(&journal_path);
    };
    if let Err(error) = prepare() {
        abandon();
        return Err(error);
    }
    let sidecar = sidecar_path(file);
    if let Err(error) = std::fs::rename(&staged_sidecar, &sidecar) {
        abandon();
        return Err(io_error(&sidecar, error));
    }
    if let Err(error) = std::fs::rename(&staged_document, file) {
        if restore_sidecar(file, previous).is_ok() {
            abandon();
        }
        return Err(io_error(file, error));
    }
    let _ = sync_dir(file);
    let _ = std::fs::remove_file(&journal_path);
    Ok(next)
}

/// Create a new document at revision 0 (refused over an existing file unless `force`).
pub fn create(file: &Path, document: &Value, force: bool) -> Result<Revision, CliError> {
    let _lock = lock(file)?;
    recover(file)?;
    if file.exists() && !force {
        return Err(
            CliError::new("file_exists", format!("{} already exists", file.display()))
                .with_path("--out")
                .with_detail(json!({ "hint": "pass --force to overwrite" })),
        );
    }
    let previous = read_sidecar(file)?;
    install(file, document, 0, previous.as_ref())
}

/// A document held under its lock, as read.
pub struct Locked {
    file: PathBuf,
    _lock: File,
    pub document: Value,
    pub digest: String,
    pub recorded: Option<Revision>,
}

pub fn open(file: &Path) -> Result<Locked, CliError> {
    let handle = lock(file)?;
    recover(file)?;
    let document = read_document(file)?;
    Ok(Locked {
        file: file.to_path_buf(),
        _lock: handle,
        digest: digest(&document)?,
        recorded: read_sidecar(file)?,
        document,
    })
}

impl Locked {
    /// The base of an edit; content changed outside simforge needs `expect_digest` naming it.
    pub fn base(
        &self,
        expect_version: Option<u64>,
        expect_digest: Option<&str>,
    ) -> Result<Revision, CliError> {
        let version = self.recorded.as_ref().map_or(0, |r| r.version);
        let base = Revision {
            version,
            digest: self.digest.clone(),
        };
        if let Some(expected) = expect_version.filter(|v| *v != version) {
            return Err(CliError::findings(
                "stale_version",
                "the document is at another revision; re-read and rebase",
            )
            .with_path("--expect-version")
            .with_detail(json!({ "expected": expected, "current": base })));
        }
        if let Some(expected) = expect_digest.filter(|d| *d != self.digest) {
            return Err(CliError::findings(
                "stale_digest",
                "the document content changed; re-read and rebase",
            )
            .with_path("--expect-digest")
            .with_detail(json!({ "expected": expected, "current": base })));
        }
        if let Some(recorded) = self.recorded.as_ref().filter(|r| r.digest != self.digest) {
            if expect_digest != Some(self.digest.as_str()) {
                return Err(CliError::findings(
                    "stale_digest",
                    format!("{} changed outside simforge since revision {}", self.file.display(), recorded.version),
                )
                .with_path(self.file.display().to_string())
                .with_detail(json!({
                    "recordedDigest": recorded.digest,
                    "current": base,
                    "hint": format!("review the change, then pass --expect-digest {} to edit it as revision {}", self.digest, recorded.version),
                })));
            }
        }
        Ok(base)
    }

    /// Commit `document` as `version` if the file still holds the base content.
    pub fn commit(
        self,
        base: &Revision,
        document: &Value,
        version: u64,
    ) -> Result<Revision, CliError> {
        if version != base.version + 1 {
            return Err(CliError::new(
                "internal",
                "a commit advances the revision by one",
            ));
        }
        let on_disk = digest(&read_document(&self.file)?)?;
        if on_disk != base.digest {
            return Err(CliError::findings(
                "stale_digest",
                "the document changed while it was being edited; re-read and rebase",
            )
            .with_path(self.file.display().to_string())
            .with_detail(json!({ "expected": base.digest, "current": on_disk })));
        }
        install(&self.file, document, version, self.recorded.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_match_the_shared_canonical_json_vectors() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/canonical-json/vectors.json");
        let vectors: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut checked = 0;
        for v in vectors["vectors"].as_array().unwrap() {
            let (Some(input), Some(expected)) = (v["input"].as_str(), v["sha256"].as_str()) else {
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<Value>(input) else {
                continue;
            };
            assert_eq!(digest(&parsed).unwrap(), expected, "{}", v["id"]);
            checked += 1;
        }
        assert!(checked >= 10, "only {checked} vectors checked");
    }

    #[test]
    fn an_edit_on_a_file_changed_outside_simforge_is_refused_until_named() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.scenario.json");
        create(&file, &json!({"a": 1}), false).unwrap();
        std::fs::write(&file, "{\"a\": 2}\n").unwrap();
        let locked = open(&file).unwrap();
        let error = locked.base(None, None).unwrap_err();
        assert_eq!(error.code, "stale_digest");
        let named = locked.digest.clone();
        let base = locked.base(None, Some(&named)).unwrap();
        assert_eq!(base.version, 0);
        let next = locked.commit(&base, &json!({"a": 3}), 1).unwrap();
        assert_eq!(next.version, 1);
        assert_eq!(open(&file).unwrap().recorded, Some(next));
    }
}
