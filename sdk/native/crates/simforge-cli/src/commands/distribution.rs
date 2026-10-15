use super::cloud::{client, seg, Client, ListArgs};
use super::fetch::{self, Target};
use crate::contract::{CliError, CmdResult, Ctx, Outcome};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// The event line a distribution download writes to stderr as it goes (contract.rs): at the
/// start, at each tenth of the archive and at least every [`PROGRESS_EVERY`], so a caller that
/// is not a terminal sees a multi-gigabyte download move too.
pub const DOWNLOAD_PROGRESS_EVENT: &str = "distribution.download.progress";
const PROGRESS_EVERY: Duration = Duration::from_secs(30);

/// Bytes of one archive written so far, reported as [`DOWNLOAD_PROGRESS_EVENT`] lines.
struct Progress<'a> {
    kind: &'a str,
    name: &'a str,
    version: &'a str,
    total: u64,
    done: u64,
    tenth: u64,
    at: Instant,
}

impl<'a> Progress<'a> {
    fn start(kind: &'a str, name: &'a str, version: &'a str, total: u64) -> Self {
        let progress = Self {
            kind,
            name,
            version,
            total,
            done: 0,
            tenth: 0,
            at: Instant::now(),
        };
        progress.report();
        progress
    }

    /// `bytes` more were written (or found already verified).
    fn add(&mut self, bytes: u64) {
        if self.advance(bytes, Instant::now()) {
            self.report();
        }
    }

    /// Count `bytes`; whether a line is due at `now` (a new tenth, or [`PROGRESS_EVERY`] since the last).
    fn advance(&mut self, bytes: u64, now: Instant) -> bool {
        self.done = (self.done + bytes).min(self.total);
        let tenth = self.done * 10 / self.total;
        if tenth > self.tenth || now.duration_since(self.at) >= PROGRESS_EVERY {
            self.tenth = tenth;
            self.at = now;
            return true;
        }
        false
    }

    fn line(&self) -> Value {
        json!({
            "event": DOWNLOAD_PROGRESS_EVENT,
            "kind": self.kind,
            "name": self.name,
            "version": self.version,
            "bytes": { "done": self.done, "total": self.total },
            "percent": self.done * 100 / self.total,
        })
    }

    fn report(&self) {
        crate::contract::event(self.line());
    }
}

#[derive(Debug, Subcommand)]
pub enum CliCommand {
    /// Download and install the staff local CLI with default-feature commands.
    Install(CliInstallArgs),
}

#[derive(Debug, Args)]
pub struct CliInstallArgs {
    #[arg(long, default_value = "x86_64-unknown-linux-gnu")]
    pub target: String,
    #[arg(long)]
    pub version: Option<String>,
    #[arg(long)]
    pub prefix: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum RuntimeCommand {
    /// Download and install the closed runtime (PolyForm Noncommercial 1.0.0) after accepting its license and SimForge's account and download terms.
    Install(InstallArgs),
    /// Report the installed runtime's protocol and engine identity.
    Status,
    /// Read the runtime license (PolyForm Noncommercial 1.0.0) and SimForge's account and download terms without accepting them.
    License,
}
#[derive(Debug, Args)]
pub struct InstallArgs {
    #[arg(long, default_value = "x86_64-unknown-linux-gnu")]
    pub target: String,
    #[arg(long)]
    pub version: Option<String>,
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Explicitly accept the current license and terms (agreement version and SHA) before downloading.
    #[arg(long)]
    pub accept_license: bool,
}
#[derive(Debug, Subcommand)]
pub enum MapsCommand {
    /// List the hosted maps your workspace can use.
    List(ListArgs),
    /// One hosted map and its versions.
    Show(super::cloud::IdArgs),
    /// Download a map your signed-in account is entitled to into the local cache, sha256-verified.
    #[cfg(feature = "local")]
    Pull(MapPullArgs),
}
#[cfg(feature = "local")]
#[derive(Debug, Args)]
pub struct MapPullArgs {
    /// SimForge-owned map name, optionally pinned as NAME@VERSION.
    pub map: String,
    /// Map cache directory. Default: the SimForge maps cache.
    #[arg(long)]
    pub cache_root: Option<PathBuf>,
    /// Explicitly accept the current license and terms, which include the map pack terms, before downloading.
    #[arg(long)]
    pub accept_license: bool,
}

pub fn runtime(command: RuntimeCommand, ctx: &Ctx) -> CmdResult {
    match command {
        RuntimeCommand::Status => Ok(Outcome::ok(crate::runtime::request(
            "handshake",
            json!({"protocol":1}),
        )?)),
        RuntimeCommand::License => Ok(Outcome::ok(client(ctx)?.get("/distribution/license")?)),
        RuntimeCommand::Install(args) => {
            let c = client(ctx)?;
            agreement(&c, args.accept_license)?;
            let mut body = json!({"target":args.target});
            if let Some(version) = args.version {
                body["version"] = json!(version);
            }
            let root = args
                .root
                .map(|p| crate::paths::absolutize(&p))
                .unwrap_or(crate::runtime::installation_root()?);
            install(
                &c,
                "/distribution/runtime",
                &body,
                "runtime",
                "simforge-runtime",
                &root,
            )
        }
    }
}
pub(crate) fn local_cli_install_error(error: CliError) -> CliError {
    if error.code == "insufficient_scope" {
        error.with_detail(json!({
            "hint": "Ask SimForge staff to grant this workspace the simforge-local CLI entitlement and publish the current staff agreement.",
            "request": "simforge-local CLI distribution access",
        }))
    } else {
        error
    }
}
pub fn cli(command: CliCommand, ctx: &Ctx) -> CmdResult {
    match command {
        CliCommand::Install(args) => {
            let c = client(ctx)?;
            agreement(&c, true)?;
            let root = args.prefix.unwrap_or_else(|| {
                std::env::var_os("HOME").map_or_else(
                    || PathBuf::from(".local/bin"),
                    |home| PathBuf::from(home).join(".local/bin"),
                )
            });
            let mut body = json!({"target": args.target});
            if let Some(version) = args.version {
                body["version"] = json!(version);
            }
            install(
                &c,
                "/distribution/cli",
                &body,
                "cli",
                "simforge-local",
                &root,
            )
            .map_err(local_cli_install_error)
        }
    }
}

pub fn maps(command: MapsCommand, ctx: &Ctx) -> CmdResult {
    let c = client(ctx)?;
    match command {
        MapsCommand::List(args) => super::cloud::page(&c, "/maps", &args),
        MapsCommand::Show(args) => Ok(Outcome::ok(c.get(&format!("/maps/{}", seg(&args.id)))?)),
        #[cfg(feature = "local")]
        MapsCommand::Pull(args) => {
            let (name, version) = args
                .map
                .split_once('@')
                .map_or((args.map.as_str(), None), |(n, v)| (n, Some(v)));
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || version == Some("")
            {
                return Err(
                    CliError::new("bad_value", "map must be NAME or NAME@VERSION").with_path("map"),
                );
            }
            agreement(&c, args.accept_license)?;
            let body = version.map_or(json!({}), |v| json!({"version":v}));
            let root = crate::paths::maps_root(args.cache_root.as_deref())?.value;
            install(
                &c,
                &format!("/distribution/maps/{}", seg(name)),
                &body,
                "map",
                name,
                &root,
            )
        }
    }
}

fn agreement(c: &Client, accept: bool) -> Result<(), CliError> {
    let doc = c.get("/distribution/license")?;
    if doc["schema"] != "simforge.distribution-agreement/v1" || doc["status"] != "available" {
        return Err(CliError::new("agreement_unavailable", "no approved distribution agreement is available for this account").with_detail(json!({"agreement":doc,"hint":"Ask SimForge for distribution eligibility; no download can proceed without an available agreement."})));
    }
    let agreement = &doc["agreement"];
    let text = field(agreement, "text")?;
    let version = field(agreement, "version")?;
    let digest = field(agreement, "sha256")?;
    if format!("{:x}", Sha256::digest(text.as_bytes())) != digest {
        return Err(CliError::new(
            "agreement_digest_mismatch",
            "agreement text does not match the server's digest",
        ));
    }
    if doc["acceptance"]["agreementVersion"] == version
        && doc["acceptance"]["agreementSha256"] == digest
    {
        return Ok(());
    }
    if !accept {
        return Err(CliError::new("agreement_acceptance_required", "read and explicitly accept the current distribution agreement before downloading").with_detail(json!({"agreement":agreement,"hint":"Read simforge runtime license, then repeat the install or map pull with --accept-license if you accept these exact terms."})));
    }
    c.post(
        "/distribution/license/accept",
        &json!({"agreementVersion":version,"agreementSha256":digest}),
    )?;
    Ok(())
}

fn field<'a>(doc: &'a Value, key: &str) -> Result<&'a str, CliError> {
    doc[key].as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
        CliError::new(
            "invalid_api_response",
            format!("distribution {key} missing"),
        )
    })
}

pub fn install(
    c: &Client,
    endpoint: &str,
    body: &Value,
    kind: &str,
    name: &str,
    root: &Path,
) -> CmdResult {
    let reply = c.post(endpoint, body)?;
    if reply["schema"] != "simforge.distribution-download/v1"
        || reply["kind"] != kind
        || reply["name"] != name
    {
        return Err(CliError::new(
            "invalid_api_response",
            "distribution archive schema or identity mismatch",
        ));
    }
    let version = field(&reply, "version")?;
    let sha = field(&reply, "sha256")?;
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(CliError::new(
            "invalid_api_response",
            "distribution sha256 invalid",
        ));
    }
    let bytes = reply["bytes"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| CliError::new("invalid_api_response", "distribution byte count missing"))?;
    signature::verify(
        &signature::trusted_keys(),
        c.host(),
        &signature::Identity {
            kind,
            name,
            target: reply["target"].as_str(),
            version,
            sha256: sha,
            bytes,
        },
        &reply["manifest"]["signature"],
    )?;
    let parent = if kind == "runtime" {
        root.parent()
            .ok_or_else(|| CliError::new("bad_value", "runtime root needs a parent directory"))?
    } else {
        root
    };
    fs::create_dir_all(parent).map_err(io)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(parent.join(".simforge-distribution.lock"))
        .map_err(io)?;
    fs4::FileExt::lock(&lock).map_err(io)?;
    let archive = parent.join(format!(".simforge-{sha}.tar.gz"));
    let mut first = Some(reply.clone());
    let mut url = || {
        let link = if let Some(link) = first.take() {
            link
        } else {
            c.post(endpoint, body)?
        };
        if link["sha256"] != sha || link["bytes"] != bytes || link["version"] != version {
            return Err(CliError::new(
                "distribution_changed",
                "archive identity changed while renewing its signed download",
            ));
        }
        let url = field(&link, "url")?;
        if !url.starts_with("https://") || url.contains('@') {
            return Err(CliError::new(
                "invalid_api_response",
                "signed archive URL must use HTTPS without user information",
            ));
        }
        Ok(url.to_owned())
    };
    let mut progress = Progress::start(kind, name, version, bytes);
    fetch::fetch(
        &mut url,
        &Target {
            path: &archive,
            sha256: sha,
            size: bytes,
        },
        &mut |written| progress.add(written),
    )?;
    let stage = parent.join(format!(
        ".simforge-stage-{}-{}",
        std::process::id(),
        &sha[..12]
    ));
    fs::create_dir(&stage).map_err(io)?;
    let result = (|| {
        extract(&archive, &stage, kind, name)?;
        if kind == "runtime" {
            if !stage.join("bin/simforge-runtime").is_file()
                || !stage.join("bin/runtime-manifest.json").is_file()
            {
                return Err(CliError::new(
                    "invalid_distribution_archive",
                    "runtime binary or manifest missing",
                ));
            }
            let manifest = verify_runtime(&stage, &reply["manifest"])?;
            fs::write(stage.join(".distribution.json"), serde_json::to_vec_pretty(&json!({"schema":"simforge.distribution-install/v1","kind":kind,"name":name,"version":version,"sha256":sha,"bytes":bytes,"agreementVersion":reply["agreementVersion"],"manifest":manifest})).unwrap()).map_err(io)?;
            replace(&stage, root)?;
        } else if kind == "cli" {
            install_cli_binary(&stage, root, &reply["manifest"]["binary"])?;
        } else {
            verify_map(&stage, &reply["manifest"], name, version)?;
            let mut moves = Vec::new();
            for family in [".corpus", "dev-assets", "map-bundles", ".map-packs"] {
                let source = stage.join(family).join(name);
                if source.exists() {
                    let destination = root.join(family).join(name);
                    fs::create_dir_all(destination.parent().unwrap()).map_err(io)?;
                    moves.push((source, destination));
                }
            }
            replace_group(&moves)?;
        }
        Ok(Outcome::ok(
            json!({"schema":"simforge.distribution-install/v1","kind":kind,"name":name,"version":version,"root":root,"sha256":sha,"bytes":bytes,"agreementVersion":reply["agreementVersion"]}),
        ))
    })();
    if stage.exists() {
        let _ = fs::remove_dir_all(&stage);
    }
    let _ = fs::remove_file(&archive);
    if let (Ok(_), Some(host)) = (&result, c.login_host()) {
        crate::runtime::report_usage(host);
    }
    result
}

fn extract(archive: &Path, stage: &Path, kind: &str, name: &str) -> Result<(), CliError> {
    let file = fs::File::open(archive).map_err(io)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut pending_path = None;
    for entry in archive.entries().map_err(io)?.raw(true) {
        let mut entry = entry.map_err(io)?;
        let ty = entry.header().entry_type();
        if ty.is_pax_local_extensions() {
            if pending_path.is_some() {
                return Err(CliError::new(
                    "invalid_distribution_archive",
                    "repeated PAX path header",
                ));
            }
            let extensions = entry.pax_extensions().map_err(io)?.ok_or_else(|| {
                CliError::new("invalid_distribution_archive", "empty PAX path header")
            })?;
            for extension in extensions {
                let extension = extension.map_err(io)?;
                if extension.key_bytes() != b"path" || pending_path.is_some() {
                    return Err(CliError::new(
                        "invalid_distribution_archive",
                        "only one PAX path record is allowed",
                    ));
                }
                let path = std::str::from_utf8(extension.value_bytes()).map_err(|_| {
                    CliError::new("invalid_distribution_archive", "PAX path is not UTF-8")
                })?;
                pending_path = Some(PathBuf::from(path));
            }
            if pending_path.is_none() {
                return Err(CliError::new(
                    "invalid_distribution_archive",
                    "PAX header has no path",
                ));
            }
            continue;
        }
        let extended = pending_path.take();
        if extended.is_some() && !(ty.is_file() || ty.is_dir()) {
            return Err(CliError::new(
                "invalid_distribution_archive",
                "PAX path must precede a regular file or directory",
            ));
        }
        let path = match &extended {
            Some(path) => path.clone(),
            None => entry.path().map_err(io)?.into_owned(),
        };
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
            || path.is_absolute()
        {
            return Err(CliError::new(
                "invalid_distribution_archive",
                "archive member path escapes its install root",
            ));
        }
        let parts: Vec<_> = path
            .components()
            .filter_map(|c| {
                if let Component::Normal(s) = c {
                    s.to_str()
                } else {
                    None
                }
            })
            .collect();
        let allowed = if parts.is_empty() && ty.is_dir() {
            true
        } else if kind == "runtime" {
            parts
                .first()
                .is_some_and(|p| ["bin", "lib", "share", "SHA256SUMS"].contains(p))
        } else if kind == "cli" {
            (parts.len() == 1 && parts[0] == "simforge-local" && ty.is_dir())
                || (parts.len() == 2
                    && parts[0] == "simforge-local"
                    && ["LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt", "simforge"]
                        .contains(&parts[1]))
        } else {
            (parts.len() == 1 && parts[0] == "SHA256SUMS")
                || (parts.first().is_some_and(|p| {
                    [".corpus", "dev-assets", "map-bundles", ".map-packs"].contains(p)
                }) && (parts.len() == 1 || parts.get(1) == Some(&name)))
        };
        if !allowed || !(ty.is_file() || ty.is_dir()) {
            return Err(CliError::new(
                "invalid_distribution_archive",
                format!(
                    "archive member {} is not an allowed regular file or directory",
                    path.display()
                ),
            ));
        }
        if extended.is_some() {
            let destination = stage.join(&path);
            fs::create_dir_all(destination.parent().unwrap()).map_err(io)?;
            entry.unpack(destination).map_err(io)?;
        } else if !entry.unpack_in(stage).map_err(io)? {
            return Err(CliError::new(
                "invalid_distribution_archive",
                "archive member cannot be installed safely",
            ));
        }
    }
    if pending_path.is_some() {
        return Err(CliError::new(
            "invalid_distribution_archive",
            "PAX path has no following file",
        ));
    }
    Ok(())
}

fn install_cli_binary(stage: &Path, prefix: &Path, declared: &Value) -> Result<(), CliError> {
    let binary = stage.join("simforge-local/simforge");
    if !binary.is_file() {
        return Err(CliError::new(
            "invalid_distribution_archive",
            "local CLI binary missing",
        ));
    }
    verify_file(&binary, declared)?;
    fs::create_dir_all(prefix).map_err(io)?;
    let next = prefix.join(format!(".simforge-{}.next", std::process::id()));
    fs::copy(&binary, &next).map_err(io)?;
    if let Err(error) = fs::rename(&next, prefix.join("simforge")) {
        let _ = fs::remove_file(&next);
        return Err(io(error));
    }
    Ok(())
}
fn replace(stage: &Path, destination: &Path) -> Result<(), CliError> {
    let backup = destination.with_file_name(format!(".simforge-backup-{}", std::process::id()));
    if backup.exists() {
        return Err(CliError::new(
            "install_conflict",
            "previous install backup exists; resolve it before replacing this installation",
        ));
    }
    let existed = destination.exists();
    if existed {
        fs::rename(destination, &backup).map_err(io)?;
    }
    if let Err(e) = fs::rename(stage, destination) {
        if existed {
            fs::rename(&backup, destination).map_err(io)?;
        }
        return Err(io(e));
    }
    if existed {
        fs::remove_dir_all(backup).map_err(io)?;
    }
    Ok(())
}
fn io(e: std::io::Error) -> CliError {
    CliError::new("install_io_error", e.to_string())
}

fn replace_group(moves: &[(PathBuf, PathBuf)]) -> Result<(), CliError> {
    let mut committed: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
    for (source, destination) in moves {
        let backup = destination.with_file_name(format!(".simforge-backup-{}", std::process::id()));
        let existed = destination.exists();
        let attempt = (|| {
            if backup.exists() {
                return Err(CliError::new(
                    "install_conflict",
                    "previous install backup exists",
                ));
            }
            if existed {
                fs::rename(destination, &backup).map_err(io)?;
            }
            if let Err(error) = fs::rename(source, destination) {
                if existed {
                    fs::rename(&backup, destination).map_err(io)?;
                }
                return Err(io(error));
            }
            Ok(())
        })();
        if let Err(error) = attempt {
            for (installed, old, had_old) in committed.into_iter().rev() {
                fs::remove_dir_all(&installed).map_err(io)?;
                if had_old {
                    fs::rename(old, installed).map_err(io)?;
                }
            }
            return Err(error);
        }
        committed.push((destination.clone(), backup, existed));
    }
    for (_, backup, existed) in committed {
        if existed {
            fs::remove_dir_all(backup).map_err(io)?;
        }
    }
    Ok(())
}

pub fn verify_runtime_binary(exe: &Path) -> Result<(), CliError> {
    let manifest_path = exe.with_file_name("runtime-manifest.json");
    if !manifest_path.exists() {
        return Ok(());
    }
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).map_err(io)?)
        .map_err(|e| CliError::new("invalid_distribution_archive", e.to_string()))?;
    if manifest["schema"] != "simforge.runtime-release/v1"
        || manifest["binary"]["name"] != "simforge-runtime"
    {
        return Err(CliError::new(
            "runtime_digest_mismatch",
            "runtime bundle manifest identity invalid",
        ));
    }
    verify_file(exe, &manifest["binary"])
}

fn verify_file(path: &Path, declared: &Value) -> Result<(), CliError> {
    let sha = field(declared, "sha256")?;
    let bytes = declared["bytes"].as_u64().ok_or_else(|| {
        CliError::new(
            "invalid_distribution_archive",
            "manifest byte count missing",
        )
    })?;
    let (actual, count) = fetch::digest_file(path)?;
    if actual != sha || count != bytes {
        return Err(CliError::new(
            "runtime_digest_mismatch",
            format!("{} differs from its signed manifest", path.display()),
        ));
    }
    Ok(())
}

fn verify_runtime(root: &Path, declared: &Value) -> Result<Value, CliError> {
    let manifest: Value =
        serde_json::from_slice(&fs::read(root.join("bin/runtime-manifest.json")).map_err(io)?)
            .map_err(|e| CliError::new("invalid_distribution_archive", e.to_string()))?;
    // The release manifest is the inner one plus what only exists outside the archive: the
    // archive's own identity and the signature over it.
    let mut expected = declared.clone();
    if let Some(object) = expected.as_object_mut() {
        object.remove("archive");
        object.remove("signature");
    }
    if manifest != expected
        || manifest["schema"] != "simforge.runtime-release/v1"
        || manifest["runtimeProtocol"] != 1
        || manifest["rendererProtocol"] != 5
    {
        return Err(CliError::new(
            "invalid_distribution_archive",
            "inner runtime manifest differs from the entitled release",
        ));
    }
    if manifest["contractSha256"]
        != format!(
            "{:x}",
            Sha256::digest(include_bytes!(
                "../../../../../contracts/runtime-protocol.v1.json"
            ))
        )
    {
        return Err(CliError::new(
            "protocol_mismatch",
            "runtime package contract digest differs from this CLI",
        ));
    }
    let binary = root.join("bin/simforge-runtime");
    verify_runtime_binary(&binary)?;
    let mut allowed: std::collections::HashSet<PathBuf> = [
        "bin/simforge-runtime",
        "bin/runtime-manifest.json",
        "SHA256SUMS",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    let components = manifest["components"].as_array().ok_or_else(|| {
        CliError::new("invalid_distribution_archive", "runtime components missing")
    })?;
    for component in components {
        let path = super::cloud::safe(field(component, "install")?)?;
        if !allowed.insert(path.clone()) {
            return Err(CliError::new(
                "invalid_distribution_archive",
                "duplicate runtime component path",
            ));
        }
        verify_file(&root.join(path), component)?;
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(io)? {
            let entry = entry.map_err(io)?;
            let path = entry.path();
            let ty = entry.file_type().map_err(io)?;
            if ty.is_dir() {
                pending.push(path);
            } else if !ty.is_file() || !allowed.contains(path.strip_prefix(root).unwrap()) {
                return Err(CliError::new(
                    "invalid_distribution_archive",
                    format!("unlisted runtime component {}", path.display()),
                ));
            }
        }
    }
    Ok(manifest)
}

fn verify_checksums(
    root: &Path,
) -> Result<std::collections::BTreeMap<PathBuf, (String, u64)>, CliError> {
    let text = fs::read_to_string(root.join("SHA256SUMS")).map_err(io)?;
    let mut files = std::collections::BTreeMap::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let (sha, name) = line.split_once("  ").ok_or_else(|| {
            CliError::new("invalid_distribution_archive", "invalid SHA256SUMS entry")
        })?;
        let path = super::cloud::safe(name)?;
        if sha.len() != 64
            || !sha.bytes().all(|b| b.is_ascii_hexdigit())
            || files.contains_key(&path)
        {
            return Err(CliError::new(
                "invalid_distribution_archive",
                "invalid or repeated archive checksum path",
            ));
        }
        let (actual, bytes) = fetch::digest_file(&root.join(&path))?;
        if actual != sha {
            return Err(CliError::new(
                "distribution_digest_mismatch",
                format!("{name} differs from SHA256SUMS"),
            ));
        }
        files.insert(path, (actual, bytes));
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(io)? {
            let entry = entry.map_err(io)?;
            let path = entry.path();
            let ty = entry.file_type().map_err(io)?;
            if ty.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                if !ty.is_file()
                    || (relative != Path::new("SHA256SUMS") && !files.contains_key(relative))
                {
                    return Err(CliError::new(
                        "invalid_distribution_archive",
                        "archive contains an unlisted checksum member",
                    ));
                }
            }
        }
    }
    Ok(files)
}

fn verify_map(root: &Path, manifest: &Value, name: &str, version: &str) -> Result<(), CliError> {
    if manifest["schema"] != "simforge.map-pack/v1"
        || manifest["name"] != name
        || manifest["version"] != version
    {
        return Err(CliError::new(
            "invalid_distribution_archive",
            "map pack manifest identity mismatch",
        ));
    }
    let checksums = verify_checksums(root)?;
    let profiles = manifest["install"]["layout"].as_object().ok_or_else(|| {
        CliError::new("invalid_distribution_archive", "map profile layout missing")
    })?;
    if !profiles.contains_key("native")
        || !profiles.contains_key("semantic")
        || (!manifest["webDigest"].is_null() && !profiles.contains_key("web"))
    {
        return Err(CliError::new(
            "invalid_distribution_archive",
            "map pack omits a required original release profile",
        ));
    }
    for (profile, layout) in profiles {
        let family = match profile.as_str() {
            "native" => ".corpus",
            "semantic" => "dev-assets",
            "web" => "map-bundles",
            _ => {
                return Err(CliError::new(
                    "invalid_distribution_archive",
                    "unknown map profile",
                ))
            }
        };
        let base = PathBuf::from(family).join(name);
        let receipt_path = base.join(".map-release.json");
        if layout.as_str() != base.to_str()
            || manifest["install"]["receipts"][profile].as_str() != receipt_path.to_str()
        {
            return Err(CliError::new(
                "invalid_distribution_archive",
                "map profile layout or receipt path differs from its install root",
            ));
        }
        let (sha, _) = checksums
            .get(&receipt_path)
            .ok_or_else(|| CliError::new("invalid_distribution_archive", "map receipt missing"))?;
        if manifest["membership"]["receiptSha256"][profile] != sha.as_str() {
            return Err(CliError::new(
                "map_receipt_digest_mismatch",
                "map receipt differs from its original release",
            ));
        }
        let receipt: Value =
            serde_json::from_slice(&fs::read(root.join(&receipt_path)).map_err(io)?)
                .map_err(|e| CliError::new("invalid_distribution_archive", e.to_string()))?;
        if receipt["schema"] != "simforge.map-installation.v1"
            || receipt["name"] != name
            || receipt["version"] != version
            || receipt["profile"] != profile.as_str()
            || receipt["releaseDigest"] != manifest["releaseDigest"]
            || receipt["canonicalDigest"] != manifest["closureDigest"]
            || receipt["webDigest"] != manifest["webDigest"]
            || receipt["derivatives"] != manifest["derivatives"]
        {
            return Err(CliError::new(
                "map_pin_mismatch",
                "installed map receipt identity differs from its entitled pack",
            ));
        }
        let members = receipt["members"].as_object().ok_or_else(|| {
            CliError::new(
                "invalid_distribution_archive",
                "map receipt members missing",
            )
        })?;
        let mut expected = std::collections::HashSet::from([receipt_path]);
        for (path, member) in members {
            let path = base.join(super::cloud::safe(path)?);
            let (actual, bytes) = checksums.get(&path).ok_or_else(|| {
                CliError::new(
                    "map_member_missing",
                    "map receipt member is absent from the pack",
                )
            })?;
            if member["sha256"] != actual.as_str() || member["bytes"].as_u64() != Some(*bytes) {
                return Err(CliError::new(
                    "map_member_digest_mismatch",
                    format!("{} differs from its original receipt", path.display()),
                ));
            }
            expected.insert(path);
        }
        if checksums
            .keys()
            .any(|path| path.starts_with(&base) && !expected.contains(path))
        {
            return Err(CliError::new(
                "invalid_distribution_archive",
                "map profile contains a member outside its original receipt",
            ));
        }
    }
    Ok(())
}

mod signature;
#[cfg(test)]
mod tests;
