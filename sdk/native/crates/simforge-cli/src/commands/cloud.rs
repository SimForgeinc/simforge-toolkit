use super::fetch;
use crate::{
    auth::{self, Source},
    contract::{CliError, CmdResult, Ctx, Outcome},
};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

fn envv(n: &str) -> Option<String> {
    std::env::var(n)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}
fn uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).unwrap();
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",b[0],b[1],b[2],b[3],b[4],b[5],b[6],b[7],b[8],b[9],b[10],b[11],b[12],b[13],b[14],b[15])
}
pub(crate) fn seg(s: &str) -> String {
    s.bytes().fold(String::new(), |mut o, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char)
        } else {
            o.push_str(&format!("%{b:02X}"))
        };
        o
    })
}
fn file_json(p: &Path) -> Result<Value, CliError> {
    let b = fs::read(p)
        .map_err(|e| CliError::new("file_unreadable", format!("{}: {e}", p.display())))?;
    serde_json::from_slice(&b)
        .map_err(|e| CliError::new("invalid_json", format!("{}: {e}", p.display())))
}
fn origin(s: &str) -> Option<String> {
    let (scheme, rest) = s.split_once("://")?;
    Some(format!(
        "{scheme}://{}",
        rest.split(['/', '?', '#']).next()?
    ))
}
/// The API acts in the credential's own workspace; a different `--workspace` is refused, never ignored.
fn workspace_matches(requested: &str, acting: Option<&str>) -> Result<(), CliError> {
    if acting == Some(requested) {
        return Ok(());
    }
    Err(CliError::new(
        "workspace_mismatch",
        match acting {
            Some(a) => format!("this credential acts in workspace {a}, not {requested}; sign in with `simforge login` and approve the CLI for {requested}'s organization, or use an API key created in {requested}"),
            None => format!("the host did not report a workspace for this credential, so {requested} cannot be confirmed"),
        },
    )
    .with_path("--workspace")
    .with_detail(json!({"requested": requested, "credentialWorkspace": acting})))
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct TemplateInput {
    template: Value,
    map_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    site_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<i64>,
    name: String,
}
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct EvaluateInput {
    scenario_id: String,
    revision_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
}
#[derive(Debug, Serialize, PartialEq)]
struct PolicyRef {
    kind: &'static str,
}
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ClosedLoopInput {
    scenario_id: String,
    revision_id: String,
    policy: PolicyRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
}
fn body<T: Serialize>(v: &T) -> Result<Value, CliError> {
    serde_json::to_value(v).map_err(|e| CliError::new("encode_error", e.to_string()))
}

#[derive(Clone)]
enum Auth {
    Key(String),
    Login(std::sync::Arc<auth::Credential>),
}
#[derive(Clone)]
pub struct Client {
    base: String,
    host: String,
    auth: Auth,
    workspace: Option<String>,
    idem: String,
}
struct Reply {
    status: u16,
    /// The host's `X-Request-Id`, which support traces a request by.
    request_id: Option<String>,
    body: Vec<u8>,
}
impl Client {
    pub fn new(c: &Ctx) -> Result<Self, CliError> {
        let raw = c.api_url.clone().or_else(|| envv("SIMFORGE_API_URL"));
        let host = auth::resolve_host(raw.as_deref().and_then(origin).as_deref())?;
        let auth = match envv("SIMFORGE_API_KEY") {
            Some(k) if k.starts_with("sfk_") => Auth::Key(k),
            Some(_) => {
                return Err(
                    CliError::new("bad_value", "SIMFORGE_API_KEY must start with sfk_")
                        .with_path("SIMFORGE_API_KEY"),
                )
            }
            None => {
                let x = auth::credential(&host)?.ok_or_else(|| auth::not_logged_in(&host))?;
                Auth::Login(std::sync::Arc::new(x))
            }
        };
        let host_name = host.name.clone();
        let origin = raw.unwrap_or(host.origin);
        let base = if origin.trim_end_matches('/').ends_with("/api/v1") {
            origin.trim_end_matches('/').to_owned()
        } else {
            format!("{}/api/v1", origin.trim_end_matches('/'))
        };
        let bound = match &auth {
            Auth::Login(x) => match x.source() {
                Source::Stored(r) => r.organization.workspace_id.clone(),
                Source::Env => None,
            },
            Auth::Key(_) => None,
        };
        let client = Self {
            base,
            host: host_name,
            auth,
            workspace: c.workspace.clone().or_else(|| bound.clone()),
            idem: c.idempotency_key.clone().unwrap_or_else(uuid),
        };
        if let Some(requested) = &c.workspace {
            let acting = match bound {
                Some(b) => Some(b),
                None => client
                    .get("/me")?
                    .pointer("/workspace/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
            workspace_matches(requested, acting.as_deref())?;
        }
        Ok(client)
    }
    /// The same client with its own Idempotency-Key, for one more independent request (MCP calls).
    pub(crate) fn fresh(&self) -> Self {
        Self {
            idem: uuid(),
            ..self.clone()
        }
    }
    /// The host's Studio origin (the API base without `/api/v1`).
    pub(crate) fn origin(&self) -> &str {
        self.base.strip_suffix("/api/v1").unwrap_or(&self.base)
    }
    /// The workspace this credential acts in: the login's own, else what the host reports.
    pub(crate) fn acting_workspace(&self) -> Result<String, CliError> {
        if let Some(w) = &self.workspace {
            return Ok(w.clone());
        }
        self.get("/me")?
            .pointer("/workspace/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                CliError::new(
                    "workspace_required",
                    "the host did not report a workspace for this credential",
                )
            })
    }
    /// The signed-in host behind this client (`None` for an API key).
    pub(crate) fn login_host(&self) -> Option<&auth::Host> {
        match &self.auth {
            Auth::Login(credential) => Some(&credential.host),
            Auth::Key(_) => None,
        }
    }
    fn token(&self) -> String {
        match &self.auth {
            Auth::Key(k) => k.clone(),
            Auth::Login(c) => c.access_token(),
        }
    }
    fn once(
        &self,
        m: &str,
        u: &str,
        b: Option<&[u8]>,
        h: &[(&str, String)],
    ) -> Result<Reply, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(concat!("simforge/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let token = self.token();
        let ws = self.workspace.clone();
        macro_rules! common {
            ($r:expr) => {{
                let mut r = $r
                    .header("Accept", "application/json")
                    .header("Authorization", format!("Bearer {token}"));
                if let Some(w) = &ws {
                    r = r.header("X-SimForge-Workspace-Id", w)
                }
                for (n, v) in h {
                    r = r.header(*n, v)
                }
                r
            }};
        }
        let r = match m {
            "GET" => common!(agent.get(u)).call(),
            "POST" => common!(agent.post(u)).send(b.unwrap_or(&[])),
            "DELETE" => common!(agent.delete(u)).call(),
            _ => return Err(format!("unsupported method {m}")),
        }
        .map_err(|e| e.to_string())?;
        let status = r.status().as_u16();
        let request_id = r
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        r.into_body()
            .into_reader()
            .take(64 * 1024 * 1024)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        Ok(Reply {
            status,
            request_id,
            body,
        })
    }
    fn req(&self, m: &str, p: &str, b: Option<&Value>, post: bool) -> Result<Value, CliError> {
        let u = if p.starts_with("http") {
            p.to_owned()
        } else {
            format!("{}{p}", self.base)
        };
        let bytes = b
            .map(|v| {
                serde_json::to_vec(v).map_err(|e| CliError::new("encode_error", e.to_string()))
            })
            .transpose()?;
        let hs = if post {
            vec![("Idempotency-Key", self.idem.clone())]
        } else {
            vec![]
        };
        for n in 0..2 {
            let r = self
                .once(m, &u, bytes.as_deref(), &hs)
                .map_err(|e| CliError::new("host_unreachable", e).with_path(p))?;
            if r.status == 401 && n == 0 {
                if let Auth::Login(c) = &self.auth {
                    let t = c.access_token();
                    if c.refresh_after_rejection(&t)?.is_some() {
                        continue;
                    }
                }
            }
            if r.status >= 500 && n == 0 {
                continue;
            }
            if !(200..300).contains(&r.status) {
                let v: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
                // `/api/v1` nests `{error: {code, reason}}`; the CLI routes (`/api/cli`) answer OAuth-style
                // `{error: "<code>", error_description}` with any detail beside it.
                let flat = v.get("error").is_some_and(Value::is_string);
                let e = if flat {
                    &v
                } else {
                    v.get("error").unwrap_or(&v)
                };
                let code = if flat { v.get("error") } else { e.get("code") }
                    .and_then(Value::as_str)
                    .unwrap_or("api_error");
                let reason = e
                    .get(if flat { "error_description" } else { "reason" })
                    .and_then(Value::as_str)
                    .unwrap_or("API request rejected");
                let mut x = if r.status < 500 {
                    CliError::findings(code, reason)
                } else {
                    CliError::new(code, reason)
                };
                if let Some(d) = e.get("detail") {
                    x = x.with_detail(d.clone())
                }
                if let Some(q) = e.get("path").and_then(Value::as_str) {
                    x = x.with_path(q)
                }
                let request_id = r.request_id.clone().or_else(|| {
                    e.get("requestId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
                return Err(
                    super::cloud_errors::refine(x, self.origin()).with_http(r.status, request_id)
                );
            }
            return if r.body.is_empty() {
                Ok(Value::Null)
            } else {
                serde_json::from_slice(&r.body)
                    .map_err(|e| CliError::new("invalid_api_response", e.to_string()))
            };
        }
        Err(CliError::new(
            "request_failed",
            "API request failed after retry",
        ))
    }
    /// The `host[:port]` this client talks to (the key its credential is stored under).
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn get(&self, p: &str) -> Result<Value, CliError> {
        self.req("GET", p, None, false)
    }
    pub fn post(&self, p: &str, v: &Value) -> Result<Value, CliError> {
        self.req("POST", p, Some(v), true)
    }
    pub fn delete(&self, p: &str) -> Result<Value, CliError> {
        self.req("DELETE", p, None, false)
    }
    pub fn upload(&self, u: &str, b: &[u8], hs: &Map<String, Value>) -> Result<(), CliError> {
        // One PUT of the whole file: a minute, plus a second per 512 KiB (a 300 MB MCAP log gets ~11 min).
        let patience = 60 + b.len() as u64 / (512 * 1024);
        let a: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(patience)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut r = a.put(u);
        for (n, v) in hs {
            if let Some(x) = v.as_str() {
                r = r.header(n, x)
            }
        }
        let s = r
            .send(b)
            .map_err(|e| CliError::new("upload_failed", e.to_string()))?
            .status()
            .as_u16();
        if !(200..300).contains(&s) {
            return Err(CliError::new("upload_failed", format!("HTTP {s}")));
        }
        Ok(())
    }
}

#[derive(Debug, Args)]
pub struct IdArgs {
    /// The id.
    pub id: String,
}
#[derive(Debug, Args)]
pub struct ListArgs {
    /// At most this many items.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Only items in this status.
    #[arg(long)]
    pub status: Option<String>,
    /// Only items of this type.
    #[arg(long)]
    pub r#type: Option<String>,
}
#[derive(Debug, Subcommand)]
pub enum ScenariosCommand {
    /// List the hosted scenarios of your workspace.
    List(ListArgs),
    /// One hosted scenario and its revisions.
    Show(IdArgs),
    /// Create a hosted scenario from a recording or a template on a map.
    Create(ScenarioCreateArgs),
}
#[derive(Debug, Args)]
pub struct ScenarioCreateArgs {
    /// A recording (`simforge.recording/v1`, or a `simforge.log-scene/v1` file), JSON or gzipped.
    #[arg(long, conflicts_with = "template")]
    pub log: Option<PathBuf>,
    /// A scenario template, instantiated at a site of the map.
    #[arg(long, conflicts_with = "log")]
    pub template: Option<PathBuf>,
    /// The map: a map version id, a map name or `name@vN`.
    #[arg(long)]
    pub map: String,
    /// The site to instantiate a template at; absent, its own pin or the first intent-preserving match.
    #[arg(long)]
    pub site: Option<String>,
    /// `simulation.seed` of the instantiated scenario.
    #[arg(long)]
    pub seed: Option<i64>,
    /// The new scenario's name.
    #[arg(long)]
    pub name: String,
    /// Explicit maximum cloud cost in cents; required for billable cloud jobs.
    #[arg(long, value_name = "CENTS")]
    pub max_cost: Option<u64>,
    #[command(flatten)]
    pub perceive: super::cloud_log::PerceiveArgs,
}
#[derive(Debug, Args)]
pub struct EvaluateCloudArgs {
    /// The hosted scenario to evaluate.
    #[arg(value_name = "SCENARIO_ID")]
    pub scenario_id: String,
    /// The scenario's immutable revision.
    #[arg(long, value_name = "REVISION_ID")]
    pub revision: String,
    /// The evaluation mode.
    #[arg(long)]
    pub mode: Option<String>,
    /// Wait for the run to finish.
    #[arg(long)]
    pub wait: bool,
    /// Explicit maximum cloud cost in cents; required unless --dry-run.
    #[arg(long, value_name = "CENTS")]
    pub max_cost: Option<u64>,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Policy {
    Scripted,
    Trajectory,
}
impl Policy {
    fn s(self) -> &'static str {
        match self {
            Self::Scripted => "scripted",
            Self::Trajectory => "trajectory",
        }
    }
}
#[derive(Debug, Args)]
pub struct ClosedLoopArgs {
    /// The hosted scenario.
    pub scenario_id: String,
    /// Its immutable revision.
    #[arg(long, value_name = "REVISION_ID")]
    pub revision: String,
    /// The policy that drives the ego.
    #[arg(long, value_enum, default_value = "scripted")]
    pub policy: Policy,
    /// Simulation steps to run.
    #[arg(long)]
    pub steps: Option<u32>,
    /// Simulation seed.
    #[arg(long)]
    pub seed: Option<u64>,
    /// Wait for the run to finish.
    #[arg(long)]
    pub wait: bool,
    /// Explicit maximum cloud cost in cents; required for billable cloud jobs.
    #[arg(long, value_name = "CENTS")]
    pub max_cost: Option<u64>,
}
#[derive(Debug, Subcommand)]
pub enum RunsCommand {
    /// List your workspace's hosted runs.
    List(ListArgs),
    /// One hosted run: status, cost, jobs and artifacts.
    Show(IdArgs),
    /// Follow a hosted run until it succeeds, fails, is partial or is cancelled.
    Watch(IdArgs),
    /// Cancel a hosted run; artifacts it already produced are kept.
    Cancel(IdArgs),
}
#[derive(Debug, Subcommand)]
pub enum ResultsCommand {
    /// Download a hosted run's artifacts, sha256-checked.
    Download(DownloadArgs),
}
#[derive(Debug, Args)]
pub struct UsageArgs {
    /// Start of the period (ISO 8601 date or time).
    #[arg(long)]
    pub from: Option<String>,
    /// End of the period (ISO 8601 date or time).
    #[arg(long)]
    pub to: Option<String>,
}
pub fn usage(c: &Client, a: UsageArgs) -> CmdResult {
    let mut q = Vec::new();
    if let Some(v) = a.from {
        q.push(format!("from={}", seg(&v)));
    }
    if let Some(v) = a.to {
        q.push(format!("to={}", seg(&v)));
    }
    let p = if q.is_empty() {
        "/usage".to_owned()
    } else {
        format!("/usage?{}", q.join("&"))
    };
    Ok(Outcome::ok(c.get(&p)?))
}
#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// The hosted run.
    pub run_id: String,
    /// Directory to write the artifacts into.
    #[arg(long, default_value = ".")]
    pub out: PathBuf,
}
pub(crate) fn page(c: &Client, p: &str, a: &ListArgs) -> CmdResult {
    let mut q = Vec::new();
    if let Some(x) = a.limit {
        q.push(format!("limit={x}"))
    }
    if let Some(x) = &a.status {
        q.push(format!("status={}", seg(x)))
    }
    if let Some(x) = &a.r#type {
        q.push(format!("type={}", seg(x)))
    }
    Ok(Outcome::ok(
        c.get(
            if q.is_empty() {
                p.to_owned()
            } else {
                format!("{p}?{}", q.join("&"))
            }
            .as_str(),
        )?,
    ))
}
pub fn scenarios(c: &Client, x: ScenariosCommand, ctx: &Ctx) -> CmdResult {
    match x {
        ScenariosCommand::List(a) => page(c, "/scenarios", &a),
        ScenariosCommand::Show(a) => Ok(Outcome::ok(c.get(&format!("/scenarios/{}", seg(&a.id)))?)),
        ScenariosCommand::Create(a) => create_scenario(c, a, ctx),
    }
}
/// Run types whose results are written by a finalize step after their last job: such a run is
/// `succeeded` (its jobs are) before its `result` exists, so a waiter also waits for the finalize.
const FINALIZING: &[&str] = &[
    "scenario_from_log",
    "scenario_from_template",
    "scenario_from_openscenario",
    "log_register",
    "log_perceive",
    "transfer",
    "map_enrichment",
];
/// How long a waiter follows a succeeded run's finalize before giving up with `run_finalize_pending`.
const FINALIZE_PATIENCE: Duration = Duration::from_secs(10 * 60);

/// Whether a waiter has the run's final answer: a terminal status and, for a finalizing type that
/// succeeded, the outcome of its finalize (`finalize` is the last `finalize_succeeded` /
/// `finalize_failed` event seen). A failed finalize flips the run to `failed`: wait for that.
fn settled(run: &Value, finalize: Option<&str>) -> bool {
    let status = run.get("status").and_then(Value::as_str).unwrap_or("");
    if !matches!(status, "succeeded" | "failed" | "partial" | "cancelled") {
        return false;
    }
    let finalizing = run
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| FINALIZING.contains(&t));
    status != "succeeded" || !finalizing || finalize == Some("finalize_succeeded")
}
fn wait(c: &Client, id: &str) -> Result<Value, CliError> {
    let mut after = 0;
    let mut finalize: Option<(String, Value)> = None;
    let mut succeeded_at: Option<std::time::Instant> = None;
    loop {
        let e = c.get(&format!("/runs/{}/events?after={after}", seg(id)))?;
        after = e.get("nextAfter").and_then(Value::as_u64).unwrap_or(after);
        for event in e
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "finalize_succeeded" | "finalize_failed") {
                finalize = Some((
                    kind.to_owned(),
                    event.get("data").cloned().unwrap_or(Value::Null),
                ));
            }
        }
        let mut r = c.get(&format!("/runs/{}", seg(id)))?;
        if settled(&r, finalize.as_ref().map(|(k, _)| k.as_str())) {
            if let Some((kind, data)) = finalize.filter(|(k, _)| k == "finalize_failed") {
                r["finalize"] = json!({ "event": kind, "detail": data });
            }
            return Ok(r);
        }
        if r.get("status").and_then(Value::as_str) == Some("succeeded") {
            let since = *succeeded_at.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() > FINALIZE_PATIENCE {
                return Err(CliError::new(
                    "run_finalize_pending",
                    format!(
                        "run {id} succeeded but its result was not written within {} minutes",
                        FINALIZE_PATIENCE.as_secs() / 60
                    ),
                )
                .with_detail(json!({ "runId": id, "hint": format!("simforge runs show {id}") })));
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
}
pub(crate) fn status(v: Value) -> Outcome {
    if matches!(
        v.get("status").and_then(Value::as_str),
        Some("failed" | "partial" | "cancelled")
    ) {
        Outcome::findings(v)
    } else {
        Outcome::ok(v)
    }
}
fn create_scenario(c: &Client, a: ScenarioCreateArgs, ctx: &Ctx) -> CmdResult {
    if let Some(p) = a.log {
        return super::cloud_log::ingest(
            c,
            ctx,
            super::cloud_log::Ingest {
                file: &p,
                map: &a.map,
                name: &a.name,
                max_cost: a.max_cost,
                perceive: &a.perceive,
                out: None,
            },
        );
    }
    if !ctx.dry_run && a.max_cost.is_none() {
        return Err(CliError::new(
            "max_cost_required",
            "cloud execution requires --max-cost CENTS",
        ));
    }
    let Some(p) = a.template else {
        return Err(CliError::new(
            "missing_argument",
            "provide --log or --template",
        ));
    };
    let input = body(&TemplateInput {
        template: file_json(&p)?,
        map_id: a.map,
        site_id: a.site,
        seed: a.seed,
        name: a.name,
    })?;
    let r = c.post(
        "/runs",
        &run_request("scenario_from_template", input, ctx.dry_run, a.max_cost),
    )?;
    if ctx.dry_run {
        return Ok(Outcome::ok(r));
    }
    let id = r
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("invalid_api_response", "run id missing"))?;
    let r = wait(c, id)?;
    // The created scenario is the run's result; a run that did not succeed reports itself as findings.
    match r.get("result") {
        Some(result)
            if !result.is_null()
                && r.get("status").and_then(Value::as_str) == Some("succeeded") =>
        {
            Ok(Outcome::ok(result.clone()))
        }
        _ => Ok(Outcome::findings(r)),
    }
}
/// The `POST /runs` body: the cost cap is a top-level field the host enforces, never part of `input`.
pub(crate) fn run_request(t: &str, input: Value, dry_run: bool, max_cost: Option<u64>) -> Value {
    let mut r = json!({"type": t, "input": input, "dryRun": dry_run});
    if let Some(cost) = max_cost {
        r["maxCostCents"] = json!(cost);
    }
    r
}
struct Submit {
    wait: bool,
    max_cost: Option<u64>,
    out: Option<PathBuf>,
}
fn run(c: &Client, ctx: &Ctx, t: &str, i: Value, s: Submit) -> CmdResult {
    if !ctx.dry_run && s.max_cost.is_none() {
        return Err(CliError::new(
            "max_cost_required",
            "cloud execution requires --max-cost CENTS",
        ));
    }
    let r = c.post("/runs", &run_request(t, i, ctx.dry_run, s.max_cost))?;
    if ctx.dry_run || !s.wait {
        return Ok(Outcome::ok(r));
    }
    let id = r
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("invalid_api_response", "run id missing"))?;
    let mut done = wait(c, id)?;
    if let Some(out) = s.out {
        if done.get("status").and_then(Value::as_str) == Some("succeeded") {
            done["download"] = download_all(c, id, &out)?;
        }
    }
    Ok(status(done))
}
pub fn evaluate(c: &Client, x: EvaluateCloudArgs, ctx: &Ctx) -> CmdResult {
    let input = body(&EvaluateInput {
        scenario_id: x.scenario_id,
        revision_id: x.revision,
        mode: x.mode,
    })?;
    run(
        c,
        ctx,
        "evaluate",
        input,
        Submit {
            wait: x.wait,
            max_cost: x.max_cost,
            out: None,
        },
    )
}
pub fn closed_loop(c: &Client, x: ClosedLoopArgs, ctx: &Ctx) -> CmdResult {
    let input = body(&ClosedLoopInput {
        scenario_id: x.scenario_id,
        revision_id: x.revision,
        policy: PolicyRef { kind: x.policy.s() },
        steps: x.steps,
        seed: x.seed,
    })?;
    run(
        c,
        ctx,
        "closed_loop",
        input,
        Submit {
            wait: x.wait,
            max_cost: x.max_cost,
            out: None,
        },
    )
}
pub fn runs(c: &Client, x: RunsCommand) -> CmdResult {
    match x {
        RunsCommand::List(a) => page(c, "/runs", &a),
        RunsCommand::Show(a) => Ok(Outcome::ok(c.get(&format!("/runs/{}", seg(&a.id)))?)),
        RunsCommand::Watch(a) => Ok(status(wait(c, &a.id)?)),
        RunsCommand::Cancel(a) => Ok(Outcome::ok(
            c.post(&format!("/runs/{}/cancel", seg(&a.id)), &json!({}))?,
        )),
    }
}
/// An artifact name as a path under `--out`: relative, `/`-separated, every part a plain name.
pub(crate) fn safe(s: &str) -> Result<PathBuf, CliError> {
    let plain =
        |part: &str| !part.is_empty() && part != "." && part != ".." && !part.contains('\\');
    if !s.starts_with('/') && s.split('/').all(plain) {
        Ok(PathBuf::from(s))
    } else {
        Err(CliError::new(
            "invalid_artifact_name",
            "artifact name must be a relative path of plain names",
        ))
    }
}
/// `results download`: every available artifact of a run, streamed to `--out` (resumable, sha256-checked).
pub fn results(c: &Client, x: ResultsCommand) -> CmdResult {
    match x {
        ResultsCommand::Download(a) => Ok(Outcome::ok(download_all(c, &a.run_id, &a.out)?)),
    }
}
fn download_all(c: &Client, run_id: &str, out: &Path) -> Result<Value, CliError> {
    let mut done = Vec::new();
    for art in run_artifacts(c, run_id)? {
        let name = art
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        done.push(download_artifact(c, &art, &out.join(safe(&name)?))?);
    }
    Ok(json!({"runId": run_id, "artifacts": done}))
}
/// The run's artifacts of the given kinds, each under `out` by its name (sha256-checked).
pub(crate) fn download_kinds(
    c: &Client,
    run_id: &str,
    out: &Path,
    kinds: &[&str],
) -> Result<Value, CliError> {
    let mut done = Vec::new();
    for art in run_artifacts(c, run_id)? {
        if !kinds.contains(&art.get("kind").and_then(Value::as_str).unwrap_or("")) {
            continue;
        }
        let name = art["name"].as_str().unwrap_or_default().to_owned();
        let mut got = download_artifact(c, &art, &out.join(safe(&name)?))?;
        got["kind"] = art["kind"].clone();
        done.push(got);
    }
    Ok(json!(done))
}
/// A run's artifacts (`{id, name, sha256, sizeBytes, ...}`), each named by its id when it has no name.
pub(crate) fn run_artifacts(c: &Client, run_id: &str) -> Result<Vec<Value>, CliError> {
    let v = c.get(&format!("/runs/{}/artifacts", seg(run_id)))?;
    v.get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut art| {
            let id = art
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| CliError::new("invalid_api_response", "artifact id missing"))?
                .to_owned();
            if art.get("name").and_then(Value::as_str).is_none() {
                art["name"] = json!(id);
            }
            Ok(art)
        })
        .collect()
}
/// Download one artifact to `path`, checked against its sha256 and size.
pub(crate) fn download_artifact(c: &Client, art: &Value, path: &Path) -> Result<Value, CliError> {
    let id = art
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("invalid_api_response", "artifact id missing"))?;
    let name = art.get("name").and_then(Value::as_str).unwrap_or(id);
    let sha256 = art
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("invalid_api_response", "sha256 missing"))?;
    let size = art
        .get("sizeBytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| CliError::new("invalid_api_response", "sizeBytes missing"))?;
    let mut url = || -> Result<String, CliError> {
        let m = c.get(&format!("/artifacts/{}/download", seg(id)))?;
        m.get("url")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CliError::new("invalid_api_response", "download url missing"))
    };
    fetch::fetch(&mut url, &fetch::Target { path, sha256, size }, &mut |_| {})
        .map_err(|e| e.with_path(name))?;
    Ok(json!({"name":name,"path":path,"sha256":sha256,"sizeBytes":size}))
}
pub fn wait_public(c: &Client, id: &str) -> Result<Value, CliError> {
    wait(c, id)
}
pub fn client(c: &Ctx) -> Result<Client, CliError> {
    Client::new(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finalizing_run_is_settled_only_once_its_finalize_succeeded() {
        let run = |t: &str, s: &str| json!({"type": t, "status": s});
        assert!(!settled(&run("render", "running"), None));
        assert!(settled(&run("render", "succeeded"), None));
        assert!(settled(&run("render", "failed"), None));
        assert!(!settled(&run("scenario_from_log", "succeeded"), None));
        assert!(settled(
            &run("scenario_from_log", "succeeded"),
            Some("finalize_succeeded")
        ));
        // A failed finalize flips the run to failed; until it shows, keep waiting.
        assert!(!settled(
            &run("transfer", "succeeded"),
            Some("finalize_failed")
        ));
        assert!(settled(&run("transfer", "failed"), Some("finalize_failed")));
        assert!(settled(&run("scenario_from_log", "cancelled"), None));
    }

    #[test]
    fn a_workspace_the_credential_does_not_act_in_is_refused() {
        assert!(workspace_matches("ws_a", Some("ws_a")).is_ok());
        let e = workspace_matches("ws_agent_qa_dev", Some("ws_simforge_ai")).unwrap_err();
        assert_eq!(e.code, "workspace_mismatch");
        assert_eq!(
            e.detail.as_ref().unwrap()["credentialWorkspace"],
            "ws_simforge_ai"
        );
        assert_eq!(
            workspace_matches("ws_a", None).unwrap_err().code,
            "workspace_mismatch"
        );
    }

    #[test]
    fn artifact_names_are_relative_paths_that_stay_under_out() {
        assert_eq!(safe("result.json").unwrap(), PathBuf::from("result.json"));
        assert_eq!(
            safe("render-2d/video/topdown.mp4").unwrap(),
            PathBuf::from("render-2d/video/topdown.mp4")
        );
        for escaping in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a//b",
            "./a",
            "a/.",
            "a\\..\\b",
        ] {
            assert_eq!(
                safe(escaping).unwrap_err().code,
                "invalid_artifact_name",
                "{escaping:?}"
            );
        }
    }
}
