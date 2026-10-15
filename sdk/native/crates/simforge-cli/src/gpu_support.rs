//! Local GPU qualification against the published support table
//! (`sdk/contracts/runtime-support.v1.json`; docs/local-runtime.md).
//!
//! 3D and schematic renders and rendered-sensor environments need a qualified NVIDIA GPU. The CLI
//! checks before it hands the job to the runtime, so unsupported hardware fails in milliseconds
//! with a named error and the table, not as a driver failure after the map has loaded. 2D renders,
//! simulation and state-only environments run on the CPU and are never checked. An explicit
//! `--allow-software-adapter` (a CPU Vulkan adapter such as lavapipe) skips the check: that render
//! is unqualified by request and its result records the software adapter.

use crate::contract::CliError;
use serde_json::{json, Value};
use std::process::Command;

const TABLE: &str = include_str!("../../../../contracts/runtime-support.v1.json");

/// One GPU as `nvidia-smi` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Gpu {
    pub index: u32,
    pub name: String,
    pub memory_mib: u64,
    pub driver: String,
}

impl Gpu {
    fn to_json(&self) -> Value {
        json!({"index": self.index, "name": self.name, "memoryMiB": self.memory_mib, "driver": self.driver})
    }
}

/// The published table (compiled in).
pub fn table() -> Value {
    serde_json::from_str(TABLE).expect("runtime-support.v1.json is valid JSON")
}

/// Parse `nvidia-smi --query-gpu=index,name,memory.total,driver_version --format=csv,noheader,nounits`.
pub fn parse_nvidia_smi(text: &str) -> Result<Vec<Gpu>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            match fields.as_slice() {
                [index, name, memory, driver] => Ok(Gpu {
                    index: index
                        .parse()
                        .map_err(|_| format!("bad GPU index in {line:?}"))?,
                    name: (*name).to_owned(),
                    memory_mib: memory
                        .parse()
                        .map_err(|_| format!("bad GPU memory in {line:?}"))?,
                    driver: (*driver).to_owned(),
                }),
                _ => Err(format!("unexpected nvidia-smi line {line:?}")),
            }
        })
        .collect()
}

fn probe() -> Result<Vec<Gpu>, String> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,name,memory.total,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .map_err(|error| format!("nvidia-smi could not run ({error})"))?;
    if !output.status.success() {
        return Err(format!(
            "nvidia-smi failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_nvidia_smi(&String::from_utf8_lossy(&output.stdout))
}

fn driver_major(driver: &str) -> Option<u64> {
    driver.split('.').next()?.parse().ok()
}

/// The GPU this machine renders on, or why it is not supported. `found` is what the probe saw.
pub fn qualify(found: Result<Vec<Gpu>, String>) -> Result<Gpu, CliError> {
    let table = table();
    let min_mib = table["gpu"]["minVramGiB"].as_u64().expect("minVramGiB") * 1024;
    let min_driver = table["gpu"]["minDriverMajor"]
        .as_u64()
        .expect("minDriverMajor");
    let requirement = format!(
        "3D and schematic rendering need an NVIDIA GPU with at least {} GiB of memory and driver {min_driver} or newer, on Linux x86_64",
        min_mib / 1024
    );
    let refuse = |code: &str, why: String, gpus: &[Gpu]| {
        CliError::new(code, format!("{requirement}; {why}")).with_detail(json!({
            "found": gpus.iter().map(Gpu::to_json).collect::<Vec<_>>(),
            "supportTable": table,
            "hint": "2D renders (--mode 2d), simulation and state-only environments (env serve --no-sensors) run without a GPU. Or render on SimForge cloud: add --cloud --max-cost CENTS.",
        }))
    };
    let gpus = match found {
        Ok(gpus) if !gpus.is_empty() => gpus,
        Ok(_) => {
            return Err(refuse(
                "gpu_unsupported",
                "nvidia-smi lists no GPU".into(),
                &[],
            ))
        }
        Err(why) => {
            return Err(refuse(
                "gpu_unsupported",
                format!("no NVIDIA GPU was found: {why}"),
                &[],
            ))
        }
    };
    let Some(best) = gpus.iter().max_by_key(|gpu| gpu.memory_mib) else {
        unreachable!("gpus is not empty")
    };
    // nvidia-smi reports a little under the marketed size (16303 MiB on a 16 GB card).
    if best.memory_mib + 512 < min_mib {
        return Err(refuse(
            "gpu_memory_unsupported",
            format!(
                "the largest GPU here, {}, has {} MiB",
                best.name, best.memory_mib
            ),
            &gpus,
        ));
    }
    if driver_major(&best.driver).is_none_or(|major| major < min_driver) {
        return Err(refuse(
            "gpu_driver_unsupported",
            format!(
                "{} runs driver {}; update the NVIDIA driver",
                best.name, best.driver
            ),
            &gpus,
        ));
    }
    Ok(best.clone())
}

/// Refuse a GPU job (`mode` 3d or schematic, or a rendered-sensor environment) on unsupported
/// hardware, and name the GPU it runs on. `argv` is the runtime argv; an explicit
/// `--allow-software-adapter` skips the check (`None`).
pub fn require_for(argv: &[String]) -> Result<Option<Gpu>, CliError> {
    if argv.iter().any(|arg| arg == "--allow-software-adapter") {
        return Ok(None);
    }
    qualify(probe()).map(Some)
}

/// The texture tier a local photoreal render gets when its argv names none: the published table's
/// default for this GPU's memory (`defaults.textures`): full textures from `fullTexturesMinVramGiB`
/// up, 512 px textures below it. `None` when the argv chooses, or the GPU takes the runtime's own
/// default (full). The caller passes it explicitly and reports it, so the choice is visible.
pub fn default_textures(gpu: &Gpu, argv: &[String]) -> Option<Value> {
    if argv
        .iter()
        .any(|arg| arg == "--textures" || arg.starts_with("--textures="))
    {
        return None;
    }
    let table = table();
    let rule = &table["defaults"]["textures"];
    let full_from = rule["fullTexturesMinVramGiB"]
        .as_u64()
        .expect("fullTexturesMinVramGiB");
    if gpu.memory_mib + 512 >= full_from * 1024 {
        return None;
    }
    let tier = rule["below"].as_str().expect("defaults.textures.below");
    Some(json!({
        "textures": tier,
        "why": format!(
            "{} has {} MiB; full textures need {full_from} GiB on the largest public maps (sdk/contracts/runtime-support.v1.json). Pass --textures uastc-full to choose them anyway.",
            gpu.name, gpu.memory_mib
        ),
    }))
}

/// Whether `env serve` argv renders sensors: a rig, unless observations are state only.
pub fn env_renders(argv: &[String]) -> bool {
    let has = |flag: &str| {
        argv.iter()
            .any(|arg| arg == flag || arg.starts_with(&format!("{flag}=")))
    };
    let state_only = argv
        .windows(2)
        .any(|pair| pair[0] == "--observation-profile" && pair[1] == "state")
        || argv.iter().any(|arg| arg == "--observation-profile=state");
    has("--rig") && !state_only
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(name: &str, mib: u64, driver: &str) -> Gpu {
        Gpu {
            index: 0,
            name: name.into(),
            memory_mib: mib,
            driver: driver.into(),
        }
    }

    #[test]
    fn nvidia_smi_csv_parses_into_gpus() {
        let gpus = parse_nvidia_smi(
            "0, NVIDIA GeForce RTX 5080, 16303, 595.84\n1, NVIDIA GeForce RTX 3080, 10240, 595.91.07\n",
        )
        .unwrap();
        assert_eq!(
            gpus[1],
            Gpu {
                index: 1,
                ..gpu("NVIDIA GeForce RTX 3080", 10240, "595.91.07")
            }
        );
        assert!(parse_nvidia_smi("0, broken\n").is_err());
    }

    #[test]
    fn qualified_hardware_passes_and_everything_else_is_refused_by_name() {
        assert_eq!(
            qualify(Ok(vec![gpu("RTX 5080", 16303, "595.84")]))
                .unwrap()
                .name,
            "RTX 5080"
        );
        assert_eq!(
            qualify(Ok(vec![gpu("RTX 4060", 7900, "595.84")]))
                .unwrap()
                .memory_mib,
            7900,
            "an 8 GB card reports under 8192 MiB"
        );
        let code = |found| qualify(found).unwrap_err().code;
        assert_eq!(
            code(Err("nvidia-smi could not run".into())),
            "gpu_unsupported"
        );
        assert_eq!(code(Ok(vec![])), "gpu_unsupported");
        assert_eq!(
            code(Ok(vec![gpu("GTX 1650", 4096, "595.84")])),
            "gpu_memory_unsupported"
        );
        assert_eq!(
            code(Ok(vec![gpu("RTX 3080", 10240, "535.104")])),
            "gpu_driver_unsupported"
        );
        let error = qualify(Ok(vec![gpu("GTX 1650", 4096, "595.84")])).unwrap_err();
        assert!(error.reason.contains("at least 8 GiB"), "{}", error.reason);
        assert_eq!(error.detail.unwrap()["found"][0]["memoryMiB"], 4096);
    }

    #[test]
    fn only_an_environment_with_a_rendered_rig_needs_the_gpu() {
        let argv = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert!(env_renders(&argv(&[
            "env", "serve", "s.json", "--rig", "rig.json"
        ])));
        assert!(!env_renders(&argv(&[
            "env",
            "serve",
            "s.json",
            "--no-sensors"
        ])));
        assert!(!env_renders(&argv(&[
            "env",
            "serve",
            "s.json",
            "--rig",
            "r.json",
            "--observation-profile",
            "state"
        ])));
    }

    #[test]
    fn an_explicit_software_adapter_is_not_checked() {
        assert_eq!(
            require_for(&["--allow-software-adapter".into()]).unwrap(),
            None
        );
    }

    #[test]
    fn a_gpu_under_the_full_texture_tier_defaults_to_512_px_textures_unless_the_render_chooses() {
        let small = gpu("RTX 3080", 10240, "595.91");
        let chosen = default_textures(&small, &[]).unwrap();
        assert_eq!(chosen["textures"], "bc7-512");
        assert!(chosen["why"].as_str().unwrap().contains("10240 MiB"));
        assert_eq!(
            default_textures(&small, &["--textures".into(), "uastc-full".into()]),
            None
        );
        assert_eq!(
            default_textures(&gpu("RTX 5080", 16303, "595.84"), &[]),
            None,
            "a 16 GB card reports 16303 MiB"
        );
    }
}
