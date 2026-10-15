//! `simforge rig`: `simforge.render-rig/v1` files, in both builds and with no
//! renderer. `init` writes a built-in rig fitted to a carrier class (a road
//! vehicle, or a drone for the drone presets), `validate` checks a file the way
//! a render reads it, and `show` says what each source captures and which way it looks.
//!
//! The built-in rigs are `sdk/catalog/sensor-rigs/fitted/<preset>.<class>.rig.json`,
//! generated from Studio's presets (`pnpm --filter @simforge/scenario
//! schema:sensor-rigs`) and embedded here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::contract::{CliError, CmdResult, Ctx, Outcome};
use crate::rig::{CameraAttributes, RenderSource, Rig};

macro_rules! fitted {
    ($($preset:literal),* $(,)?) => {
        &[$(
            ($preset, "car", include_str!(concat!("../../../../../catalog/sensor-rigs/fitted/", $preset, ".car.rig.json"))),
            ($preset, "truck", include_str!(concat!("../../../../../catalog/sensor-rigs/fitted/", $preset, ".truck.rig.json"))),
            ($preset, "van", include_str!(concat!("../../../../../catalog/sensor-rigs/fitted/", $preset, ".van.rig.json"))),
        )*]
    };
}

macro_rules! fitted_drone {
    ($($preset:literal),* $(,)?) => {
        &[$(
            ($preset, "drone", include_str!(concat!("../../../../../catalog/sensor-rigs/fitted/", $preset, ".drone.rig.json"))),
        )*]
    };
}

/// `(preset, vehicle class, rig file)`, every road rig on every road class's default box.
const FITTED: &[(&str, &str, &str)] = fitted!(
    "basic-dash-camera",
    "tesla-hw3",
    "waymo-5th-gen",
    "surround-7cam-1lidar",
    "roof-8cam-6lidar-4radar",
    "alpamayo-2cam",
    "alpamayo-4cam",
    "alpamayo-6cam",
    "alpamayo-6cam-vqa",
    "front-4cam-wide-cross-tele",
);

/// `(preset, "drone", rig file)`, every drone rig on the drone's default box (Training capture: 24 fps).
const FITTED_DRONE: &[(&str, &str, &str)] = fitted_drone!(
    "drone-inspection-wide",
    "drone-inspection-wide-zoom",
    "drone-mapping-nadir",
    "drone-fpv-forward",
);

/// The actor the built-in rigs are fitted on.
const FITTED_ACTOR: &str = "ego";

fn fitted_rigs() -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)> {
    FITTED.iter().chain(FITTED_DRONE.iter())
}

fn presets() -> Vec<&'static str> {
    let mut ids: Vec<&str> = fitted_rigs().map(|(id, _, _)| *id).collect();
    ids.dedup();
    ids
}

#[derive(Debug, Subcommand)]
pub enum RigCommand {
    /// Write a built-in rig fitted to a carrier class's default box as a `simforge.render-rig/v1` file.
    Init(InitArgs),
    /// Check a `simforge.render-rig/v1` file as a render reads it; the installed runtime plans a camera lens (cameraModel). Exit 2 with the finding.
    Validate(FileArgs),
    /// What a rig captures: each source's mount, the way it faces and its field of view, and the cameras' horizontal coverage; the installed runtime shows a rig with a camera lens.
    Show(FileArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VehicleClass {
    Car,
    Truck,
    Van,
    /// The drone presets' carrier.
    Drone,
}

impl VehicleClass {
    fn as_str(self) -> &'static str {
        match self {
            VehicleClass::Car => "car",
            VehicleClass::Truck => "truck",
            VehicleClass::Van => "van",
            VehicleClass::Drone => "drone",
        }
    }
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// The built-in rig.
    #[arg(long, value_name = "PRESET", value_parser = clap::builder::PossibleValuesParser::new(presets()))]
    pub preset: String,
    /// The carrier class whose default box the mounts are fitted to: car, truck or van for a road rig, drone for a drone rig.
    #[arg(
        long = "class",
        value_enum,
        value_name = "CLASS",
        default_value = "car"
    )]
    pub class: VehicleClass,
    /// The actor named in the file (every source's `actorId`); a render re-hosts a placeholder on the scenario's ego and refuses a file naming another of its roles.
    #[arg(long, value_name = "ID", default_value = FITTED_ACTOR)]
    pub actor: String,
    /// Where to write the rig.
    #[arg(long, short = 'o', value_name = "FILE")]
    pub out: PathBuf,
    /// Overwrite an existing file.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct FileArgs {
    /// A `simforge.render-rig/v1` file.
    #[arg(value_name = "RIG.JSON")]
    pub rig: PathBuf,
}

pub fn run(command: RigCommand, _ctx: &Ctx) -> CmdResult {
    match command {
        RigCommand::Init(args) => init(args),
        RigCommand::Validate(args) => validate(&args.rig),
        RigCommand::Show(args) => show(&args.rig),
    }
}

fn init(args: InitArgs) -> CmdResult {
    if args.out.exists() && !args.force {
        return Err(CliError::new(
            "file_exists",
            format!("{} already exists", args.out.display()),
        )
        .with_path("--out")
        .with_detail(json!({ "hint": "pass --force to overwrite" })));
    }
    if args.actor.trim().is_empty() {
        return Err(CliError::new("bad_value", "the actor id is empty").with_path("--actor"));
    }
    let class = args.class.as_str();
    let preset = args.preset.as_str();
    let (_, _, text) = fitted_rigs()
        .find(|(id, c, _)| *id == preset && *c == class)
        .ok_or_else(|| {
            let drone_rig = FITTED_DRONE.iter().any(|(id, _, _)| *id == preset);
            CliError::new(
                "bad_value",
                if drone_rig {
                    format!("{preset} is a drone rig: fit it with --class drone")
                } else {
                    format!("{preset} is a road rig: fit it with --class car, truck or van")
                },
            )
            .with_path("--class")
        })?;
    let mut rig: Value = serde_json::from_str(text).expect("built-in rigs are JSON");
    for source in rig["sources"]
        .as_array_mut()
        .expect("built-in rigs list sources")
    {
        source["actorId"] = json!(args.actor);
    }
    let parsed = Rig::parse(&rig)?;
    let mut bytes = serde_json::to_vec_pretty(&rig).expect("JSON values serialize");
    bytes.push(b'\n');
    std::fs::write(&args.out, bytes).map_err(|e| {
        CliError::new(
            "write_failed",
            format!("cannot write {}: {e}", args.out.display()),
        )
        .with_path("--out")
    })?;
    Ok(Outcome::ok(json!({
        "rig": args.out.display().to_string(),
        "preset": preset,
        "class": class,
        "actorId": args.actor,
        "sources": parsed.sources.len(),
        "modalities": modalities(&parsed),
    })))
}

/// The parsed rig, or the runtime's answer to `rig <verb>` when a camera
/// declares a lens: planning a lens is the closed runtime's.
fn parse_or_forward(path: &Path, verb: &str) -> Result<Result<Rig, Outcome>, CliError> {
    let rig = Rig::parse(&crate::rig::read_file(path, "RIG.JSON")?)
        .map_err(|e| e.with_path(path.display().to_string()))?;
    if !rig.has_lens() {
        return Ok(Ok(rig));
    }
    let argv = ["rig", verb, &path.display().to_string()].map(String::from);
    crate::runtime::command(&argv).map(Err).map_err(|e| {
        if e.code == "runtime_not_installed" {
            e.with_detail(json!({ "hint": "a camera lens (cameraModel) is planned by the SimForge runtime: install it with `simforge runtime install`" }))
        } else {
            e
        }
    })
}

fn validate(path: &Path) -> CmdResult {
    let rig = match parse_or_forward(path, "validate")? {
        Ok(rig) => rig,
        Err(outcome) => return Ok(outcome),
    };
    Ok(Outcome::ok(json!({
        "rig": path.display().to_string(),
        "valid": true,
        "sources": rig.sources.len(),
        "modalities": modalities(&rig),
        "actors": actors(&rig),
    })))
}

fn show(path: &Path) -> CmdResult {
    let rig = match parse_or_forward(path, "show")? {
        Ok(rig) => rig,
        Err(outcome) => return Ok(outcome),
    };
    let cameras: Vec<(f64, f64)> = rig
        .sources
        .iter()
        .filter_map(|s| match s {
            RenderSource::Camera(c) => Some((
                c.common.transform.rotation.yaw_rad.to_degrees(),
                camera_fields(&c.attributes).0,
            )),
            _ => None,
        })
        .collect();
    Ok(Outcome::ok(json!({
        "rig": path.display().to_string(),
        "frame": "actor-local metres: +x forward, +y up, +z left; yaw counter-clockwise seen from above, pitch up",
        "actors": actors(&rig),
        "modalities": modalities(&rig),
        "sources": rig.sources.iter().map(source_summary).collect::<Vec<_>>(),
        "cameraCoverage": coverage(&cameras),
        "sensorHosts": match &rig.sensor_hosts {
            Some(hosts) => serde_json::to_value(hosts).expect("hosts serialize"),
            None => json!("each source's own actorId"),
        },
        "clip": rig.clip.map(|c| json!({ "startSeconds": c.start_seconds, "endSeconds": c.end_seconds })),
        "video": rig.video.as_ref().map(|v| serde_json::to_value(v).expect("video serializes")),
    })))
}

fn modalities(rig: &Rig) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for source in &rig.sources {
        *counts.entry(source.modality().to_owned()).or_insert(0) += 1;
    }
    counts
}

fn actors(rig: &Rig) -> Vec<String> {
    let mut ids: Vec<String> = rig
        .sources
        .iter()
        .map(|s| s.common().actor_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// A pinhole camera's horizontal and vertical field of view (degrees); a
/// rig with a lens is shown by the runtime.
fn camera_fields(a: &CameraAttributes) -> (f64, f64, &'static str) {
    let h = a
        .horizontal_fov_deg
        .expect("a lens-free parsed camera has horizontalFovDeg");
    let v = 2.0 * ((h.to_radians() / 2.0).tan() * a.height as f64 / a.width as f64).atan();
    (h, v.to_degrees(), "pinhole")
}

/// Which way a mount faces, from its yaw (degrees, counter-clockwise from forward).
fn facing(yaw_deg: f64) -> &'static str {
    const SECTORS: [&str; 8] = [
        "front",
        "front-left",
        "left",
        "rear-left",
        "rear",
        "rear-right",
        "right",
        "front-right",
    ];
    let sector = ((yaw_deg.rem_euclid(360.0) + 22.5) / 45.0).floor() as usize % 8;
    SECTORS[sector]
}

fn round(v: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (v * scale).round() / scale
}

fn source_summary(source: &RenderSource) -> Value {
    let common = source.common();
    let (p, r) = (common.transform.position, common.transform.rotation);
    let yaw = r.yaw_rad.to_degrees();
    let mut summary = json!({
        "outputName": common.output_name,
        "modality": source.modality(),
        "actorId": common.actor_id,
        "sensorId": common.sensor_id,
        "label": common.sensor_label,
        "position": { "x": p.x, "y": p.y, "z": p.z },
        "facing": facing(yaw),
        "yawDeg": round(yaw, 2),
        "pitchDeg": round(r.pitch_rad.to_degrees(), 2),
        "rollDeg": round(r.roll_rad.to_degrees(), 2),
    });
    summary["sensor"] = match source {
        RenderSource::Camera(s) => {
            let a = &s.attributes;
            let (h, v, lens) = camera_fields(a);
            json!({
                "width": a.width,
                "height": a.height,
                "fps": a.fps,
                "lens": lens,
                "horizontalFovDeg": round(h, 2),
                "verticalFovDeg": round(v, 2),
                "nearM": a.near_m,
                "farM": a.far_m,
            })
        }
        RenderSource::Lidar(s) => serde_json::to_value(&s.attributes).expect("lidar serializes"),
        RenderSource::Radar(s) => serde_json::to_value(&s.attributes).expect("radar serializes"),
    };
    summary
}

/// The azimuths the cameras see, as seen from one point (the offsets between
/// mounts are ignored): covered degrees and the uncovered gaps, counter-clockwise
/// from forward.
fn coverage(cameras: &[(f64, f64)]) -> Value {
    if cameras.is_empty() {
        return json!({ "cameras": 0, "coveredDeg": 0.0, "gaps": [[-180.0, 180.0]] });
    }
    // Intervals on [0, 360), each camera's split where it wraps.
    let mut intervals = Vec::new();
    for &(yaw, hfov) in cameras {
        if hfov >= 360.0 {
            intervals.push((0.0, 360.0));
            continue;
        }
        let start = (yaw - hfov / 2.0).rem_euclid(360.0);
        let end = start + hfov;
        if end > 360.0 {
            intervals.push((start, 360.0));
            intervals.push((0.0, end - 360.0));
        } else {
            intervals.push((start, end));
        }
    }
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (start, end) in intervals {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let covered: f64 = merged.iter().map(|(s, e)| e - s).sum();
    let mut gaps = Vec::new();
    for (i, &(_, end)) in merged.iter().enumerate() {
        let next = if i + 1 < merged.len() {
            merged[i + 1].0
        } else {
            merged[0].0 + 360.0
        };
        if next - end > 1e-9 {
            let signed = |deg: f64| round(if deg > 180.0 { deg - 360.0 } else { deg }, 2);
            gaps.push(json!([
                signed(end.rem_euclid(360.0)),
                signed(next.rem_euclid(360.0))
            ]));
        }
    }
    json!({ "cameras": cameras.len(), "coveredDeg": round(covered.min(360.0), 2), "gaps": gaps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_rig_is_embedded_and_parses() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../catalog/sensor-rigs/fitted");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        on_disk.sort();
        let mut embedded: Vec<String> = fitted_rigs()
            .map(|(id, class, _)| format!("{id}.{class}.rig.json"))
            .collect();
        embedded.sort();
        assert_eq!(
            embedded,
            on_disk,
            "FITTED lists every file in {}",
            dir.display()
        );
        for (id, class, text) in fitted_rigs() {
            let rig = Rig::parse(&serde_json::from_str(text).unwrap())
                .unwrap_or_else(|e| panic!("{id}.{class}: {e}"));
            assert_eq!(actors(&rig), vec![FITTED_ACTOR.to_owned()]);
        }
        // Drone rigs record the drone carrier's 24 fps Training capture and ride its gimbal where they say so.
        for (id, _, text) in FITTED_DRONE {
            let rig = Rig::parse(&serde_json::from_str(text).unwrap()).unwrap();
            assert!(
                rig.sources
                    .iter()
                    .filter_map(|s| s.rgb())
                    .all(|c| c.attributes.fps == 24.0),
                "{id}"
            );
        }
        let nadir = FITTED_DRONE
            .iter()
            .find(|(id, _, _)| *id == "drone-mapping-nadir")
            .unwrap()
            .2;
        let nadir = Rig::parse(&serde_json::from_str(nadir).unwrap()).unwrap();
        assert_eq!(
            nadir.sources[0].common().mount_frame,
            Some(crate::rig::MountFrame::Gimbal)
        );
    }

    #[test]
    fn facing_names_the_sector_a_mount_looks_into() {
        assert_eq!(facing(0.0), "front");
        assert_eq!(facing(22.0), "front");
        assert_eq!(facing(45.0), "front-left");
        assert_eq!(facing(90.0), "left");
        assert_eq!(facing(180.0), "rear");
        assert_eq!(facing(-180.0), "rear");
        assert_eq!(facing(-90.0), "right");
        assert_eq!(facing(-30.0), "front-right");
    }

    #[test]
    fn coverage_merges_overlaps_and_reports_gaps_across_the_rear() {
        // Front 120 and left/right 90: covered from -105 to 105 degrees.
        let c = coverage(&[(0.0, 120.0), (60.0, 90.0), (-60.0, 90.0)]);
        assert_eq!(c["coveredDeg"], 210.0);
        assert_eq!(c["gaps"], json!([[105.0, -105.0]]));
        // A rear camera wrapping through 180 leaves two gaps.
        let c = coverage(&[(0.0, 90.0), (180.0, 90.0)]);
        assert_eq!(c["coveredDeg"], 180.0);
        assert_eq!(c["gaps"], json!([[45.0, 135.0], [-135.0, -45.0]]));
        assert_eq!(coverage(&[(0.0, 360.0)])["gaps"], json!([]));
    }
}
