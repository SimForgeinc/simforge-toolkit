use crate::contract::{CliError, CmdResult, Ctx};
use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RenderMode {
    #[value(name = "2d")]
    TwoD,
    Schematic,
    #[value(name = "3d")]
    ThreeD,
}
impl RenderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TwoD => "2d",
            Self::Schematic => "schematic",
            Self::ThreeD => "3d",
        }
    }
}

#[derive(Debug, Args)]
pub struct ExecutionArgs {
    /// Scenario JSON file or materialized workspace; identical input for local and cloud.
    pub target: PathBuf,
    /// Output folder; local requires it, cloud waits and downloads when it is given.
    #[arg(long, short = 'o')]
    pub out: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "2d")]
    pub mode: RenderMode,
    /// Sensor rig JSON (`simforge rig init`). Default: the `basic-dash-camera` rig on the ego.
    #[arg(long)]
    pub rig: Option<PathBuf>,
    /// Render preset.
    #[arg(long, value_parser = ["training", "showcase", "schematic"])]
    pub preset: Option<String>,
    /// Wait for a submitted cloud job to finish.
    #[arg(long)]
    pub wait: bool,
    /// Download a succeeded cloud job into --out; implies --wait.
    #[arg(long, requires = "out")]
    pub download: bool,
    /// Explicit maximum cloud cost in cents; required for billable cloud jobs.
    #[arg(long, value_name = "CENTS")]
    pub max_cost: Option<u64>,
    /// Immutable revision for an existing hosted scenario target.
    #[arg(long)]
    pub revision: Option<String>,
    /// Runtime-specific options; forwarded without shell interpolation.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra: Vec<String>,
}

#[derive(Debug, Args)]
pub struct PreviewArgs {
    /// Scenario JSON file or materialized workspace; identical input for local and cloud.
    pub target: PathBuf,
    /// Output file (PNG for a frame, MP4 for a clip).
    #[arg(long, short = 'o')]
    pub out: PathBuf,
    #[arg(long, value_enum, default_value = "2d")]
    pub mode: RenderMode,
    /// Sensor rig JSON (`simforge rig init`). Default: the `basic-dash-camera` rig on the ego.
    #[arg(long)]
    pub rig: Option<PathBuf>,
    /// The rig camera to look through.
    #[arg(long)]
    pub camera: Option<String>,
    /// Scenario time in seconds.
    #[arg(long, default_value_t = 0.0)]
    pub at: f64,
    /// Clip length in seconds.
    #[arg(long)]
    pub seconds: Option<f64>,
    /// Render preset.
    #[arg(long, value_parser = ["training", "showcase", "schematic"])]
    pub preset: Option<String>,
    /// Overwrite an existing output file.
    #[arg(long)]
    pub force: bool,
    /// Explicit maximum cloud cost in cents; required for billable cloud jobs.
    #[arg(long, value_name = "CENTS")]
    pub max_cost: Option<u64>,
    /// Immutable revision for an existing hosted scenario target.
    #[arg(long)]
    pub revision: Option<String>,
    /// Wait for a submitted cloud job to finish.
    #[arg(long)]
    pub wait: bool,
    /// Download a succeeded cloud job into --out; implies --wait.
    #[arg(long)]
    pub download: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra: Vec<String>,
}

pub fn operation_argv(argv: &[String]) -> Vec<String> {
    let mut index = 0;
    while index < argv.len() && argv[index].starts_with('-') {
        let word = &argv[index];
        index += 1;
        if matches!(
            word.as_str(),
            "--workspace" | "--api-url" | "--idempotency-key" | "--map-version"
        ) {
            index += 1;
        }
    }
    argv[index.min(argv.len())..].to_vec()
}

#[derive(Debug, Subcommand)]
pub enum PreviewCommand {
    /// One still frame of a scenario at --at seconds (PNG), locally or with --cloud.
    Frame(PreviewArgs),
    /// A short clip of a scenario from --at for --seconds (MP4), locally or with --cloud.
    Clip(PreviewArgs),
}

pub fn local_argv(argv: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for word in argv {
        if skip {
            skip = false;
            continue;
        }
        if word == "--" {
            out.push(word.clone());
            continue;
        }
        if matches!(
            word.as_str(),
            "--json" | "--cloud" | "--pretty" | "--wait" | "--download" | "--dry-run"
        ) {
            continue;
        }
        if matches!(
            word.as_str(),
            "--max-cost"
                | "--revision"
                | "--mode"
                | "--api-url"
                | "--workspace"
                | "--idempotency-key"
                | "--map-version"
        ) {
            skip = true;
            continue;
        }
        if [
            "--max-cost=",
            "--revision=",
            "--mode=",
            "--api-url=",
            "--workspace=",
            "--idempotency-key=",
            "--map-version=",
        ]
        .iter()
        .any(|prefix| word.starts_with(prefix))
        {
            continue;
        }
        out.push(word.clone());
    }
    out
}

pub fn run(args: ExecutionArgs, ctx: &Ctx, op: &str) -> CmdResult {
    if ctx.cloud {
        if !ctx.dry_run && args.max_cost.is_none() {
            return Err(CliError::new(
                "max_cost_required",
                "cloud execution requires an explicit --max-cost in cents",
            ));
        }
        match op {
            "render" => super::unified_cloud::cloud_render(&args, ctx),
            "simulate" => super::unified_cloud::cloud_simulate(&args, ctx),
            _ => unreachable!(),
        }
    } else {
        if args.out.is_none() {
            return Err(
                CliError::new("missing_argument", "local execution requires --out DIR")
                    .with_path("--out"),
            );
        }
        if args.max_cost.is_some() || args.revision.is_some() {
            return Err(CliError::new(
                "cloud_flag_requires_cloud",
                "--max-cost and --revision require --cloud",
            ));
        }
        if ctx.dry_run {
            return Err(CliError::new(
                "cloud_flag_requires_cloud",
                "--dry-run requires --cloud",
            ));
        }
        let mut argv = local_argv(&ctx.argv);
        let mut defaults = None;
        if op == "render" && !matches!(args.mode, RenderMode::TwoD) {
            let gpu = crate::gpu_support::require_for(&argv)?;
            if matches!(args.mode, RenderMode::ThreeD) {
                defaults = gpu.and_then(|gpu| crate::gpu_support::default_textures(&gpu, &argv));
            }
        }
        if let Some(chosen) = &defaults {
            argv.extend([
                "--textures".to_owned(),
                chosen["textures"].as_str().expect("tier").to_owned(),
            ]);
        }
        let mut outcome = crate::runtime::operation(op, &argv[1..], Some(args.mode.as_str()))?;
        if let Some(chosen) = defaults {
            outcome.value["localDefaults"] = chosen;
        }
        Ok(outcome)
    }
}

pub fn preview(command: PreviewCommand, ctx: &Ctx) -> CmdResult {
    let clip = matches!(&command, PreviewCommand::Clip(_));
    let args = match command {
        PreviewCommand::Frame(a) | PreviewCommand::Clip(a) => a,
    };
    if !matches!(args.mode, RenderMode::TwoD) && (args.rig.is_none() || args.camera.is_none()) {
        return Err(CliError::new(
            "missing_argument",
            "3d and schematic previews require --rig and --camera",
        ));
    }
    if !clip && args.seconds.is_some() {
        return Err(CliError::new(
            "bad_value",
            "--seconds applies to preview clip",
        ));
    }
    if ctx.cloud {
        if !ctx.dry_run && args.max_cost.is_none() {
            return Err(CliError::new(
                "max_cost_required",
                "cloud preview requires an explicit --max-cost in cents",
            ));
        }
        super::unified_cloud::cloud_preview(&args, clip, ctx)
    } else {
        let mut argv = local_argv(&ctx.argv);
        let mut defaults = None;
        if !matches!(args.mode, RenderMode::TwoD) {
            let gpu = crate::gpu_support::require_for(&argv)?;
            if matches!(args.mode, RenderMode::ThreeD) {
                defaults = gpu.and_then(|gpu| crate::gpu_support::default_textures(&gpu, &argv));
            }
        }
        if let Some(chosen) = &defaults {
            argv.extend([
                "--textures".to_owned(),
                chosen["textures"].as_str().expect("tier").to_owned(),
            ]);
        }
        let mut outcome =
            crate::runtime::operation("preview", &argv[1..], Some(args.mode.as_str()))?;
        if let Some(chosen) = defaults {
            outcome.value["localDefaults"] = chosen;
        }
        super::preview_identity::attach(outcome, &args.target)
    }
}
