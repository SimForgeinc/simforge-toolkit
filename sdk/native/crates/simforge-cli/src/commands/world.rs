use crate::contract::{CliError, CmdResult, Ctx};
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum WorldCommand {
    /// Serve a persistent local realtime world and its authenticated drive/state endpoints.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    #[arg(long)]
    pub map: PathBuf,
    #[arg(long)]
    pub scenario: Option<PathBuf>,
    #[arg(long)]
    pub seed: Option<u64>,
    #[arg(long, value_parser = ["realtime", "fast", "paused"])]
    pub pacing: Option<String>,
    #[arg(long)]
    pub socket: Option<PathBuf>,
    #[arg(long)]
    pub listen: Option<String>,
    #[arg(long)]
    pub token_file: PathBuf,
    #[arg(long)]
    pub vehicles: Option<u32>,
    #[arg(long)]
    pub pedestrians: Option<u32>,
    #[arg(long)]
    pub record: Option<PathBuf>,
    #[arg(long)]
    pub journal_dir: Option<PathBuf>,
    #[arg(long)]
    pub ready_file: Option<PathBuf>,
    #[arg(long)]
    pub restore: Option<PathBuf>,
    #[arg(long)]
    pub cache_root: Option<PathBuf>,
    #[arg(long)]
    pub allow_no_colliders: bool,
    /// Explicit browser origins allowed by the closed host (repeatable).
    #[arg(long)]
    pub allow_origin: Vec<String>,
}

pub fn run(_command: WorldCommand, ctx: &Ctx) -> CmdResult {
    if ctx.cloud {
        return Err(CliError::new(
            "operation_unavailable",
            "persistent world serving is local-only in this version",
        ));
    }
    crate::runtime::serve_world(&super::execution::local_argv(&ctx.argv))
}
