use crate::contract::{CliError, CmdResult, Ctx};
use clap::{Args, Subcommand};

pub mod challenge;
pub mod cloud;
pub mod cloud_drafts;
pub mod cloud_errors;
pub mod cloud_log;
pub mod datasets;
mod datasets_verify;
pub mod distribution;
pub mod engine_info;
pub mod execution;
pub(crate) mod fetch;
pub mod keys;
pub mod login;
pub mod mcp;
pub mod open_loop;
mod preview_identity;
pub mod rig;
pub mod scenario_pull;
pub mod skills;
pub mod telemetry;
pub mod unified_cloud;
pub mod vehicle;
pub mod world;

pub const PLANNED: &[&str] = &[];

#[derive(Debug, Args)]
pub struct RuntimeArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub argv: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in to a SimForge host (browser approval, or --device for a URL and code).
    Login(login::LoginArgs),
    /// Sign out of a host: revoke this machine's session and delete its stored tokens.
    Logout(login::HostArgs),
    #[command(subcommand)]
    Auth(login::AuthCommand),
    /// The signed-in account, organization, workspace and scopes.
    Whoami(login::HostArgs),
    #[command(subcommand)]
    Runtime(distribution::RuntimeCommand),
    #[command(subcommand)]
    Cli(distribution::CliCommand),
    #[command(subcommand)]
    Maps(distribution::MapsCommand),
    /// Render the same scenario file locally or with --cloud.
    Render(execution::ExecutionArgs),
    /// Simulate the same scenario file locally or with --cloud.
    Simulate(execution::ExecutionArgs),
    #[command(subcommand)]
    Preview(execution::PreviewCommand),
    /// Author scenario files with atomic typed operations, locally or with --cloud.
    Scenario(RuntimeArgs),
    /// Expose this command surface as MCP tools over stdio.
    Mcp(mcp::McpArgs),
    #[command(subcommand)]
    Rig(rig::RigCommand),
    #[command(subcommand)]
    Vehicle(vehicle::VehicleCommand),
    #[command(subcommand)]
    Skills(skills::SkillsCommand),
    /// The installed runtime's usage reports (a condition of SimForge's account and download terms).
    #[command(subcommand)]
    Telemetry(telemetry::TelemetryCommand),
    /// Hosted usage and spend of the signed-in workspace.
    Usage(cloud::UsageArgs),
    /// Create, list and revoke the workspace's API keys.
    #[command(subcommand)]
    Keys(keys::KeysCommand),
    #[command(subcommand)]
    Drafts(cloud_drafts::DraftsCommand),
    #[command(subcommand)]
    Authoring(cloud_drafts::AuthoringCommand),
    #[command(subcommand)]
    Scenarios(cloud::ScenariosCommand),
    #[command(subcommand)]
    Datasets(datasets::DatasetsCommand),
    #[command(subcommand)]
    Runs(cloud::RunsCommand),
    #[command(subcommand)]
    Results(cloud::ResultsCommand),
    /// Run a hosted closed-loop evaluation of a scripted or trajectory policy on a scenario revision.
    ClosedLoop(cloud::ClosedLoopArgs),
    /// Hosted open-loop Alpamayo 1.5 evaluation of a succeeded cloud scenario render.
    #[command(subcommand)]
    OpenLoop(open_loop::OpenLoopCommand),
    /// The SimForge driving challenge: validate, submit and follow a driver image.
    #[command(subcommand)]
    Challenge(challenge::ChallengeCommand),
    #[cfg(feature = "local")]
    Doctor(RuntimeArgs),
    EngineInfo(RuntimeArgs),
    #[cfg(feature = "local")]
    Assets(RuntimeArgs),
    Timeline(RuntimeArgs),
    Bench(RuntimeArgs),
    Import(RuntimeArgs),
    Recording(RuntimeArgs),
    Log(RuntimeArgs),
    #[cfg(feature = "local")]
    Env(RuntimeArgs),
    #[command(subcommand)]
    World(world::WorldCommand),
    #[cfg(feature = "local")]
    Package(RuntimeArgs),
    Sites(RuntimeArgs),
    Instantiate(RuntimeArgs),
    Template(RuntimeArgs),
    Locations(RuntimeArgs),
    Variation(RuntimeArgs),
    Transfer(RuntimeArgs),
    Batch(RuntimeArgs),
    Evaluate(RuntimeArgs),
    Evidence(RuntimeArgs),
    Cell(RuntimeArgs),
    Validate(RuntimeArgs),
    Export(RuntimeArgs),
    Catalog(RuntimeArgs),
}

pub fn is_runtime_command(name: &str) -> bool {
    matches!(
        name,
        "scenario"
            | "engine-info"
            | "timeline"
            | "bench"
            | "import"
            | "recording"
            | "log"
            | "sites"
            | "instantiate"
            | "template"
            | "locations"
            | "variation"
            | "transfer"
            | "batch"
            | "evaluate"
            | "evidence"
            | "cell"
            | "validate"
            | "export"
            | "catalog"
    ) || is_local_runtime_command(name)
}

#[cfg(feature = "local")]
fn is_local_runtime_command(name: &str) -> bool {
    matches!(name, "doctor" | "assets" | "env" | "package")
}

#[cfg(not(feature = "local"))]
fn is_local_runtime_command(_name: &str) -> bool {
    false
}

pub fn dispatch(command: Command, ctx: &Ctx) -> CmdResult {
    match command {
        Command::Login(args) => login::login(args, ctx),
        Command::Logout(args) => login::logout(args, ctx),
        Command::Auth(args) => login::run_auth(args, ctx),
        Command::Whoami(args) => login::whoami(args, ctx),
        Command::Runtime(args) => distribution::runtime(args, ctx),
        Command::Cli(args) => distribution::cli(args, ctx),
        Command::Maps(args) => distribution::maps(args, ctx),
        Command::Render(args) => execution::run(args, ctx, "render"),
        Command::Simulate(args) => execution::run(args, ctx, "simulate"),
        Command::Preview(args) => execution::preview(args, ctx),
        Command::Scenario(args) if args.argv.first().map(String::as_str) == Some("pull") => {
            scenario_pull::run(&execution::local_argv(&args.argv[1..]), ctx)
        }
        Command::Scenario(args) if ctx.cloud => {
            unified_cloud::cloud_scenario(&execution::local_argv(&args.argv), ctx)
        }
        Command::Mcp(args) => mcp::run(args, ctx),
        Command::Rig(args) => rig::run(args, ctx),
        Command::Vehicle(args) => vehicle::run(args, ctx),
        Command::Skills(args) => skills::run(args, ctx),
        Command::Telemetry(args) => telemetry::run(args),
        Command::World(args) => world::run(args, ctx),
        Command::Usage(args) => cloud::usage(&cloud::client(ctx)?, args),
        Command::Keys(args) => keys::run(&cloud::client(ctx)?, args),
        Command::Transfer(args) if ctx.cloud => {
            unified_cloud::transfer::cloud_transfer(&args.argv, ctx)
        }
        Command::Import(args) if ctx.cloud => cloud_log::import_cloud(&args.argv, ctx),
        Command::Drafts(args) => cloud_drafts::drafts(&cloud::client(ctx)?, args, ctx),
        Command::Authoring(args) => cloud_drafts::authoring(&cloud::client(ctx)?, args),
        Command::Scenarios(args) => cloud::scenarios(&cloud::client(ctx)?, args, ctx),
        Command::Datasets(args) => datasets::run(args, ctx),
        Command::Challenge(args) => challenge::run(args, ctx),
        Command::Runs(args) => cloud::runs(&cloud::client(ctx)?, args),
        Command::Results(args) => cloud::results(&cloud::client(ctx)?, args),
        Command::Evaluate(args) if ctx.cloud => {
            use clap::Parser;
            #[derive(Parser)]
            struct Input {
                #[command(flatten)]
                args: cloud::EvaluateCloudArgs,
            }
            let parsed =
                Input::try_parse_from(std::iter::once("evaluate".to_owned()).chain(args.argv))
                    .map_err(|error| crate::parse_error(&error, &ctx.argv))?;
            cloud::evaluate(&cloud::client(ctx)?, parsed.args, ctx)
        }
        Command::ClosedLoop(args) => {
            if !ctx.dry_run && args.max_cost.is_none() {
                return Err(CliError::new(
                    "max_cost_required",
                    "cloud execution requires --max-cost",
                ));
            }
            cloud::closed_loop(&cloud::client(ctx)?, args, ctx)
        }
        Command::OpenLoop(args) => open_loop::run(args, ctx),
        _ => {
            if ctx.cloud {
                return Err(CliError::new(
                    "operation_unavailable",
                    "this operation has no cloud implementation; use its local runtime path",
                ));
            }
            let mut argv = execution::local_argv(&ctx.argv);
            if argv.first().map(String::as_str) == Some("doctor")
                && !argv.iter().any(|arg| {
                    arg == "--registry" || arg.starts_with("--registry=") || arg == "--offline"
                })
            {
                argv.push("--offline".into());
            }
            if argv.first().map(String::as_str) == Some("env")
                && argv.get(1).map(String::as_str) == Some("serve")
            {
                if crate::gpu_support::env_renders(&argv) {
                    crate::gpu_support::require_for(&argv)?;
                }
                crate::runtime::serve_env(&argv)
            } else {
                crate::runtime::command(&argv)
            }
        }
    }
}
