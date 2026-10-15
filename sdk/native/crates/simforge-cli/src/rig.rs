//! `simforge.render-rig/v1` files: the sources a render captures and the
//! actors that carry them. A rig's `sources` are exactly the hosted render
//! intent's `renderSpec.sources` (`RenderSourceV3`, `@simforge/scenario`)
//! and its `sensorHosts` the intent's `sensorHosts`, so a rig copied out of a
//! hosted render request renders unchanged. Schema:
//! `render/render-rig.v1.schema.json`.
//!
//! Parsing checks the file against the render-spec/v3 ranges (the ones zod
//! enforces) with no renderer: `simforge rig` runs it in both builds. An
//! f-theta lens is checked for shape only; planning it is the closed
//! runtime's (`simforge rig validate|show` hand a rig with a lens to the
//! runtime). What one engine supports on top (the native engine's
//! modalities, sizes and budgets) is checked where that engine renders.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::contract::CliError;

pub const RENDER_RIG_V1_SCHEMA: &str = "simforge.render-rig/v1";

fn invalid(code: &str, reason: impl Into<String>) -> CliError {
    CliError::findings(code, reason)
}

/// A rig file's JSON (`flag` names it in errors); [`Rig::parse`] checks it.
pub fn read_file(path: &Path, flag: &str) -> Result<Value, CliError> {
    let bytes = std::fs::read(path).map_err(|e| {
        CliError::new(
            "missing_file",
            format!("cannot read {flag} {}: {e}", path.display()),
        )
        .with_path(flag)
    })?;
    serde_json::from_slice(&bytes).map_err(|e| {
        CliError::findings("render_rig_invalid", format!("{flag} is not JSON: {e}")).with_path(flag)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rotation {
    #[serde(default)]
    pub yaw_rad: f64,
    #[serde(default)]
    pub pitch_rad: f64,
    #[serde(default)]
    pub roll_rad: f64,
}

/// `SensorMountSchema`: actor-local metres, +X forward, +Y up, +Z left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub position: Vec3,
    #[serde(default)]
    pub rotation: Rotation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraAttributes {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Pinhole field of view; exclusive with `cameraModel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizontal_fov_deg: Option<f64>,
    pub near_m: f64,
    pub far_m: f64,
    /// Lens projection (`{"type": "ftheta", ...}`, an f-theta calibration);
    /// exclusive with `horizontalFovDeg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_model: Option<CameraModel>,
}

/// A camera's lens calibration on the wire (`{"type": "ftheta", ...}`, the
/// rig schema's `cameraModel`). This crate checks its shape; the closed
/// runtime plans it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CameraModel {
    /// The f-theta polynomial lens.
    Ftheta(FthetaCalibration),
}

/// F-theta intrinsics: a pixel-distance-to-angle polynomial about the
/// principal point of a reference image, plus an optional linear skew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FthetaCalibration {
    pub reference_resolution: [u32; 2],
    pub principal_point: [f64; 2],
    pub pixeldist_to_angle_poly: Vec<f64>,
    pub max_angle_rad: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub angle_to_pixeldist_poly: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_cde: Option<[f64; 3]>,
}

impl CameraModel {
    /// The calibration's shape: positive reference size, finite numbers and
    /// a non-empty polynomial.
    fn check(&self) -> Result<(), String> {
        let CameraModel::Ftheta(f) = self;
        if f.reference_resolution.contains(&0) {
            return Err("referenceResolution must be positive".into());
        }
        let numbers = f
            .principal_point
            .iter()
            .chain(&f.pixeldist_to_angle_poly)
            .chain(&f.angle_to_pixeldist_poly)
            .chain(f.linear_cde.iter().flatten())
            .chain([&f.max_angle_rad]);
        if !numbers.into_iter().all(|v| v.is_finite()) {
            return Err("every calibration number must be finite".into());
        }
        if f.pixeldist_to_angle_poly.is_empty() {
            return Err("pixeldistToAnglePoly is empty".into());
        }
        Ok(())
    }
}

fn default_lidar_hfov() -> f64 {
    360.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LidarAttributes {
    pub channels: u32,
    pub range_m: f64,
    pub points_per_second: u64,
    pub rotation_frequency_hz: f64,
    pub upper_fov_deg: f64,
    pub lower_fov_deg: f64,
    /// Azimuth coverage; the schema default is a full 360-degree revolution.
    #[serde(default = "default_lidar_hfov")]
    pub horizontal_fov_deg: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RadarAttributes {
    pub horizontal_fov_deg: f64,
    pub vertical_fov_deg: f64,
    pub range_m: f64,
    pub points_per_second: u64,
}

/// What a camera's mount rides (`RenderCameraMountFrame`): the host body (default) or the host's
/// sampled world-stabilized gimbal (drone flight).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountFrame {
    Body,
    Gimbal,
}

/// The fields every source shares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCommon {
    pub actor_id: String,
    pub sensor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_label: Option<String>,
    pub output_name: String,
    pub transform: Mount,
    /// Cameras only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_frame: Option<MountFrame>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Source<A> {
    pub common: SourceCommon,
    /// `rgb | depth | semantic | instance` for cameras, else `lidar` / `radar`.
    pub modality: String,
    pub attributes: A,
}

/// `RenderSourceV3`.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderSource {
    Camera(Source<CameraAttributes>),
    Lidar(Source<LidarAttributes>),
    Radar(Source<RadarAttributes>),
}

impl RenderSource {
    pub fn common(&self) -> &SourceCommon {
        match self {
            RenderSource::Camera(s) => &s.common,
            RenderSource::Lidar(s) => &s.common,
            RenderSource::Radar(s) => &s.common,
        }
    }

    pub fn output_name(&self) -> &str {
        &self.common().output_name
    }

    pub fn modality(&self) -> &str {
        match self {
            RenderSource::Camera(s) => &s.modality,
            RenderSource::Lidar(_) => "lidar",
            RenderSource::Radar(_) => "radar",
        }
    }

    /// The RGB cameras (the native engine renders only `rgb` camera sources).
    pub fn rgb(&self) -> Option<&Source<CameraAttributes>> {
        match self {
            RenderSource::Camera(s) if s.modality == "rgb" => Some(s),
            _ => None,
        }
    }

    pub fn to_json(&self) -> Value {
        let (modality, attributes) = match self {
            RenderSource::Camera(s) => (s.modality.as_str(), serde_json::to_value(&s.attributes)),
            RenderSource::Lidar(s) => ("lidar", serde_json::to_value(&s.attributes)),
            RenderSource::Radar(s) => ("radar", serde_json::to_value(&s.attributes)),
        };
        let mut v = serde_json::to_value(self.common()).expect("source serializes");
        v["modality"] = json!(modality);
        v["attributes"] = attributes.expect("attributes serialize");
        v
    }
}

fn parse_source(index: usize, value: &Value) -> Result<RenderSource, CliError> {
    let path = format!("sources[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| invalid("render_rig_invalid", format!("{path} is not an object")))?;
    let known = [
        "actorId",
        "sensorId",
        "sensorLabel",
        "outputName",
        "transform",
        "mountFrame",
        "modality",
        "attributes",
    ];
    if let Some(unknown) = object.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(invalid(
            "render_rig_invalid",
            format!("{path} has unknown field {unknown}"),
        ));
    }
    let modality = object
        .get("modality")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("render_rig_invalid", format!("{path}.modality is missing")))?
        .to_owned();
    let mut common_json = value.clone();
    common_json
        .as_object_mut()
        .expect("object")
        .remove("modality");
    let attributes = common_json
        .as_object_mut()
        .expect("object")
        .remove("attributes")
        .ok_or_else(|| {
            invalid(
                "render_rig_invalid",
                format!("{path}.attributes is missing"),
            )
        })?;
    let err = |what: &str, e: serde_json::Error| {
        invalid("render_rig_invalid", format!("{path}.{what}: {e}"))
    };
    let common: SourceCommon = serde_json::from_value(common_json).map_err(|e| err("", e))?;
    let source = match modality.as_str() {
        "rgb" | "depth" | "semantic" | "instance" => RenderSource::Camera(Source {
            common,
            modality,
            attributes: serde_json::from_value(attributes).map_err(|e| err("attributes", e))?,
        }),
        "lidar" => RenderSource::Lidar(Source {
            common,
            modality,
            attributes: serde_json::from_value(attributes).map_err(|e| err("attributes", e))?,
        }),
        "radar" => RenderSource::Radar(Source {
            common,
            modality,
            attributes: serde_json::from_value(attributes).map_err(|e| err("attributes", e))?,
        }),
        other => {
            return Err(invalid(
                "render_rig_invalid",
                format!("{path}.modality {other:?} (rgb|depth|semantic|instance|lidar|radar)"),
            ))
        }
    };
    validate_source(&path, &source)?;
    Ok(source)
}

fn is_output_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// The render-spec/v3 schema's range checks (the ones zod enforces).
fn validate_source(path: &str, source: &RenderSource) -> Result<(), CliError> {
    let fail = |why: String| Err(invalid("render_rig_invalid", format!("{path}: {why}")));
    let c = source.common();
    if !is_output_name(&c.output_name) {
        return fail(format!(
            "outputName {:?} must match ^[A-Za-z0-9][A-Za-z0-9_-]{{0,63}}$",
            c.output_name
        ));
    }
    if c.actor_id.is_empty() || c.sensor_id.is_empty() {
        return fail("actorId and sensorId are required".into());
    }
    let r = c.transform.rotation;
    let (half, quarter) = (std::f64::consts::PI, std::f64::consts::FRAC_PI_2);
    if r.yaw_rad.abs() > half || r.roll_rad.abs() > half || r.pitch_rad.abs() > quarter {
        return fail(
            "transform.rotation is outside yaw/roll [-pi, pi], pitch [-pi/2, pi/2]".into(),
        );
    }
    let p = c.transform.position;
    if ![p.x, p.y, p.z].iter().all(|v| v.is_finite()) {
        return fail("transform.position must be finite".into());
    }
    if c.mount_frame.is_some() && !matches!(source, RenderSource::Camera(_)) {
        return fail("mountFrame is a camera's; lidar and radar ride the body".into());
    }
    match source {
        RenderSource::Camera(s) => {
            let a = &s.attributes;
            if !(64..=8192).contains(&a.width) || !(64..=8192).contains(&a.height) {
                return fail(format!(
                    "camera size {}x{} is outside 64..8192",
                    a.width, a.height
                ));
            }
            if !(a.fps > 0.0 && a.fps <= 240.0) {
                return fail(format!("fps {} is outside (0, 240]", a.fps));
            }
            match (a.horizontal_fov_deg, &a.camera_model) {
                (Some(hfov), None) => {
                    if !(hfov > 0.0 && hfov <= 180.0) {
                        return fail(format!("horizontalFovDeg {hfov} is outside (0, 180]"));
                    }
                }
                (None, Some(model)) => {
                    if let Err(error) = model.check() {
                        return fail(format!("cameraModel: {error}"));
                    }
                }
                _ => {
                    return fail(
                        "a camera declares exactly one of horizontalFovDeg and cameraModel".into(),
                    )
                }
            }
            if !(a.near_m > 0.0 && a.far_m.is_finite() && a.far_m > a.near_m) {
                return fail("nearM must be positive and farM greater than nearM".into());
            }
        }
        RenderSource::Lidar(s) => {
            let a = &s.attributes;
            if !(1..=256).contains(&a.channels)
                || !(a.range_m > 0.0 && a.range_m <= 1000.0)
                || a.points_per_second == 0
            {
                return fail("lidar channels 1..256, rangeM (0, 1000] and a positive pointsPerSecond are required".into());
            }
            if !(a.rotation_frequency_hz > 0.0 && a.rotation_frequency_hz <= 240.0) {
                return fail(format!(
                    "rotationFrequencyHz {} is outside (0, 240]",
                    a.rotation_frequency_hz
                ));
            }
            if a.upper_fov_deg <= a.lower_fov_deg
                || a.upper_fov_deg.abs() > 180.0
                || a.lower_fov_deg.abs() > 180.0
            {
                return fail(
                    "upperFovDeg must be greater than lowerFovDeg, both within [-180, 180]".into(),
                );
            }
            if !(5.0..=360.0).contains(&a.horizontal_fov_deg) {
                return fail(format!(
                    "horizontalFovDeg {} is outside [5, 360]",
                    a.horizontal_fov_deg
                ));
            }
        }
        RenderSource::Radar(s) => {
            let a = &s.attributes;
            if !(a.horizontal_fov_deg > 0.0 && a.horizontal_fov_deg <= 180.0)
                || !(a.vertical_fov_deg > 0.0 && a.vertical_fov_deg <= 180.0)
                || !(a.range_m > 0.0 && a.range_m <= 1000.0)
                || a.points_per_second == 0
            {
                return fail("radar fields are outside the render-spec ranges".into());
            }
        }
    }
    Ok(())
}

/// `RenderSensorSourceHost`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SensorHost {
    pub source_id: String,
    pub actor_id: String,
    /// `{catalogAssetId, ...}`; extra keys are kept (the hosted schema is loose here).
    pub vehicle_asset: Value,
}

/// `RenderClipSchema`: `[startSeconds, endSeconds)` of timeline time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Clip {
    pub start_seconds: f64,
    pub end_seconds: f64,
}

/// `RenderVideoV3Schema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Video {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub container: String,
    pub codec: String,
    pub quality: String,
}

/// A parsed `simforge.render-rig/v1` document.
#[derive(Debug, Clone, PartialEq)]
pub struct Rig {
    pub sources: Vec<RenderSource>,
    /// Absent in the file: every source is hosted by its own `actorId`
    /// ([`Rig::sensor_hosts`] derives them, with the host's catalog id).
    pub sensor_hosts: Option<Vec<SensorHost>>,
    /// Absent: the caller renders the whole timeline and says so.
    pub clip: Option<Clip>,
    pub video: Option<Video>,
}

impl Rig {
    /// Whether a camera declares a lens calibration, which only the runtime plans.
    pub fn has_lens(&self) -> bool {
        self.sources
            .iter()
            .any(|s| matches!(s, RenderSource::Camera(c) if c.attributes.camera_model.is_some()))
    }

    pub fn parse(value: &Value) -> Result<Self, CliError> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid("render_rig_invalid", "the rig is not a JSON object"))?;
        if value["schema"] != RENDER_RIG_V1_SCHEMA {
            return Err(invalid(
                "render_rig_invalid",
                format!(
                    "rig schema {} (expected {RENDER_RIG_V1_SCHEMA})",
                    value["schema"]
                ),
            ));
        }
        let known = ["schema", "sources", "sensorHosts", "clip", "video"];
        if let Some(unknown) = object.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(invalid(
                "render_rig_invalid",
                format!("unknown rig field {unknown}"),
            ));
        }
        let list = value["sources"]
            .as_array()
            .filter(|l| !l.is_empty() && l.len() <= 64)
            .ok_or_else(|| invalid("render_rig_invalid", "sources must list 1..64 sources"))?;
        let sources = list
            .iter()
            .enumerate()
            .map(|(i, s)| parse_source(i, s))
            .collect::<Result<Vec<_>, _>>()?;
        let mut names = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for s in &sources {
            if !names.insert(s.output_name().to_owned()) {
                return Err(invalid(
                    "render_rig_invalid",
                    format!("duplicate source outputName {:?}", s.output_name()),
                ));
            }
            let c = s.common();
            if !keys.insert((
                c.actor_id.clone(),
                c.sensor_id.clone(),
                s.modality().to_owned(),
            )) {
                return Err(invalid(
                    "render_rig_invalid",
                    "duplicate actor/sensor/modality capture source",
                ));
            }
        }
        let parse = |key: &str| -> Result<Option<Value>, CliError> { Ok(object.get(key).cloned()) };
        let sensor_hosts: Option<Vec<SensorHost>> = match parse("sensorHosts")? {
            None => None,
            Some(v) => Some(
                serde_json::from_value(v)
                    .map_err(|e| invalid("render_rig_invalid", format!("sensorHosts: {e}")))?,
            ),
        };
        if let Some(hosts) = &sensor_hosts {
            for host in hosts {
                if host.vehicle_asset["catalogAssetId"]
                    .as_str()
                    .is_none_or(|s| s.trim().is_empty())
                {
                    return Err(invalid(
                        "render_rig_invalid",
                        format!(
                            "sensorHosts {}: vehicleAsset.catalogAssetId is required",
                            host.source_id
                        ),
                    ));
                }
            }
        }
        let clip: Option<Clip> = match parse("clip")? {
            None => None,
            Some(v) => Some(
                serde_json::from_value(v)
                    .map_err(|e| invalid("render_rig_invalid", format!("clip: {e}")))?,
            ),
        };
        let video: Option<Video> = match parse("video")? {
            None => None,
            Some(v) => Some(
                serde_json::from_value(v)
                    .map_err(|e| invalid("render_rig_invalid", format!("video: {e}")))?,
            ),
        };
        Ok(Self {
            sources,
            sensor_hosts,
            clip,
            video,
        })
    }

    /// The rig's sensor hosts: the file's, else one per source hosted by its
    /// own `actorId`, whose vehicle asset is that actor's timeline catalog id
    /// (`catalog_of(actorId)`; an actor the timeline lacks is refused).
    pub fn sensor_hosts(
        &self,
        catalog_of: impl Fn(&str) -> Option<String>,
    ) -> Result<(Vec<SensorHost>, bool), CliError> {
        if let Some(hosts) = &self.sensor_hosts {
            return Ok((hosts.clone(), false));
        }
        let hosts = self
            .sources
            .iter()
            .map(|s| {
                let actor = &s.common().actor_id;
                let catalog = catalog_of(actor).ok_or_else(|| {
                    invalid("native_sensor_host_unknown", format!("source {} is mounted on actor {actor}, which the timeline does not have", s.output_name()))
                })?;
                Ok(SensorHost {
                    source_id: s.output_name().to_owned(),
                    actor_id: actor.clone(),
                    vehicle_asset: json!({ "catalogAssetId": catalog }),
                })
            })
            .collect::<Result<Vec<_>, CliError>>()?;
        Ok((hosts, true))
    }
}

/// `sdk/catalog/sensor-rigs/presets.json` (`simforge.sensor-rig-presets/v1`), generated from the
/// TypeScript presets: the anchored templates, each render profile's capture, the default actor
/// boxes and the rig a render mounts when none is named.
const PRESETS_JSON: &str = include_str!("../../../../catalog/sensor-rigs/presets.json");

/// What a camera of a mounted preset records.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

/// An actor's box, metres (`DEFAULT_ACTOR_DIMS` when the scenario names none).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActorBox {
    pub length: f64,
    pub width: f64,
    pub height: f64,
}

fn presets() -> &'static Value {
    static PRESETS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    PRESETS.get_or_init(|| serde_json::from_str(PRESETS_JSON).expect("presets.json is JSON"))
}

/// The rig a render mounts on the ego when the request names none (`DEFAULT_RENDER_RIG`).
pub fn default_preset_id() -> &'static str {
    presets()["defaultRig"]
        .as_str()
        .expect("presets.json names its default rig")
}

/// A render profile's camera capture (`RENDER_PROFILES[profile].video`), or on a drone carrier its
/// drone capture (`droneFps`: milestone-1 drone RGB is 24 fps, Training's ground rate stays 20).
pub fn profile_capture(profile: &str, drone: bool) -> Result<Capture, CliError> {
    let c = &presets()[if drone { "droneCaptures" } else { "captures" }][profile];
    match (c["width"].as_u64(), c["height"].as_u64(), c["fps"].as_f64()) {
        (Some(w), Some(h), Some(fps)) => Ok(Capture {
            width: w as u32,
            height: h as u32,
            fps,
        }),
        _ => Err(CliError::new(
            "bad_value",
            format!("no render profile {profile:?}"),
        )),
    }
}

/// The default box of an actor class (`DEFAULT_ACTOR_DIMS`).
pub fn default_box(class: &str) -> Option<ActorBox> {
    let d = &presets()["actorDims"][class];
    Some(ActorBox {
        length: d["length"].as_f64()?,
        width: d["width"].as_f64()?,
        height: d["height"].as_f64()?,
    })
}

/// `rounded` in sensor-rigs.ts: six decimals, no negative zero.
fn rounded(v: f64) -> f64 {
    let r = (v * 1_000_000.0).round() / 1_000_000.0;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

/// `anchorPosition` + `resolveSensorRigMount` in sensor-rigs.ts.
fn anchored_position(mount: &Value, b: ActorBox) -> Result<Value, CliError> {
    let bad = || {
        CliError::new(
            "internal",
            format!("presets.json mount {mount} is not anchored"),
        )
    };
    let a = &mount["anchor"];
    let x = match a["longitudinal"].as_str().ok_or_else(bad)? {
        "front" => b.length / 2.0,
        "rear" => -b.length / 2.0,
        _ => 0.0,
    };
    let y = match a["vertical"].as_str().ok_or_else(bad)? {
        "top" => b.height,
        "center" => b.height / 2.0,
        _ => 0.0,
    };
    let z = match a["lateral"].as_str().ok_or_else(bad)? {
        "left" => b.width / 2.0,
        "right" => -b.width / 2.0,
        _ => 0.0,
    };
    let o = &mount["offset"];
    let f = |v: &Value| v.as_f64().ok_or_else(bad);
    Ok(
        json!({ "x": rounded(x + f(&o["x"])?), "y": rounded(y + f(&o["y"])?), "z": rounded(z + f(&o["z"])?) }),
    )
}

/// `outputName` in render-rig.ts: the sensor id in the output-name alphabet.
fn output_name(sensor_id: &str) -> String {
    sensor_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect()
}

/// `presetRenderRig`: a built-in preset mounted on an actor's box as a `simforge.render-rig/v1`
/// on `actor_id`, every camera recording `capture` (the TypeScript planner's mount, generated
/// into presets.json; `fitted/*.rig.json` are its parity fixtures).
pub fn preset_rig(
    preset_id: &str,
    actor_id: &str,
    actor_box: ActorBox,
    capture: Capture,
) -> Result<Rig, CliError> {
    let preset = presets()["presets"]
        .as_array()
        .and_then(|all| all.iter().find(|p| p["id"] == preset_id))
        .ok_or_else(|| {
            CliError::new(
                "render_rig_preset_unknown",
                format!("no built-in sensor rig {preset_id:?}"),
            )
        })?;
    let defaults = &presets()["sourceDefaults"];
    let mut sources = Vec::new();
    for sensor in preset["sensors"].as_array().into_iter().flatten() {
        if sensor["enabled"] == false {
            continue;
        }
        let id = sensor["id"].as_str().ok_or_else(|| {
            CliError::new(
                "internal",
                format!("presets.json {preset_id}: a sensor has no id"),
            )
        })?;
        let mut common = json!({
            "actorId": actor_id,
            "sensorId": id,
            "outputName": output_name(id),
            "transform": {
                "position": anchored_position(&sensor["mount"], actor_box)?,
                "rotation": sensor["mount"]["rotation"],
            },
        });
        if let Some(label) = sensor["label"]
            .as_str()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            common["sensorLabel"] = json!(label);
        }
        let (modality, attributes) = match sensor["type"].as_str() {
            Some("dash_camera") => {
                if sensor["mountFrame"] == "gimbal" {
                    common["mountFrame"] = json!("gimbal");
                }
                let c = &sensor["camera"];
                (
                    "rgb",
                    json!({ "width": capture.width, "height": capture.height, "fps": capture.fps, "horizontalFovDeg": c["horizontalFovDeg"], "nearM": c["nearM"], "farM": c["farM"] }),
                )
            }
            Some("lidar") => {
                let f = &sensor["field"];
                let v = f["verticalFovDeg"].as_f64().ok_or_else(|| {
                    CliError::new(
                        "internal",
                        format!("presets.json {preset_id}/{id}: lidar has no verticalFovDeg"),
                    )
                })?;
                let mut a = defaults["lidar"].clone();
                a["rangeM"] = f["farM"].clone();
                a["upperFovDeg"] = json!(v / 2.0);
                a["lowerFovDeg"] = json!(-v / 2.0);
                a["horizontalFovDeg"] = f["horizontalFovDeg"].clone();
                ("lidar", a)
            }
            Some("radar") => {
                let f = &sensor["field"];
                let mut a = defaults["radar"].clone();
                a["horizontalFovDeg"] = f["horizontalFovDeg"].clone();
                a["verticalFovDeg"] = f["verticalFovDeg"].clone();
                a["rangeM"] = f["farM"].clone();
                ("radar", a)
            }
            other => {
                return Err(CliError::new(
                    "internal",
                    format!("presets.json {preset_id}/{id}: sensor type {other:?}"),
                ))
            }
        };
        common["modality"] = json!(modality);
        common["attributes"] = attributes;
        sources.push(common);
    }
    Rig::parse(&json!({ "schema": RENDER_RIG_V1_SCHEMA, "sources": sources }))
}

/// What a preview renders: one frame, or a clip of whole seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PreviewSpan {
    /// The camera's frame at `at` seconds.
    Frame { at: f64 },
    /// `seconds` of video from `at`.
    Clip { at: f64, seconds: f64 },
}

/// The rig a preview renders: the one RGB camera `camera` (its `outputName`)
/// of `rig`, with the span as the rig's clip and, for a clip, the camera's own
/// size and rate as the video. Every other source is left out; a name that is
/// not an RGB camera of the rig is `preview_camera_unknown`, listing them.
pub fn preview_rig(rig: &Value, camera: &str, span: PreviewSpan) -> Result<Value, CliError> {
    let parsed = Rig::parse(rig)?;
    let cameras: Vec<&Source<CameraAttributes>> = parsed
        .sources
        .iter()
        .filter_map(RenderSource::rgb)
        .collect();
    let Some(source) = cameras.iter().find(|s| s.common.output_name == camera) else {
        let names: Vec<&str> = cameras
            .iter()
            .map(|s| s.common.output_name.as_str())
            .collect();
        return Err(CliError::new(
            "preview_camera_unknown",
            format!(
                "the rig has no RGB camera {camera:?}; its cameras: {}",
                names.join(", ")
            ),
        )
        .with_path("--camera")
        .with_detail(json!({ "cameras": names })));
    };
    let fps = source.attributes.fps;
    let (at, seconds) = match span {
        PreviewSpan::Frame { at } => (at, 1.0 / fps),
        PreviewSpan::Clip { at, seconds } => (at, seconds),
    };
    if !(at.is_finite() && at >= 0.0) {
        return Err(CliError::new(
            "bad_value",
            format!("--at {at} is not a time in seconds from the start"),
        )
        .with_path("--at"));
    }
    if !(seconds.is_finite() && seconds > 0.0) {
        return Err(CliError::new(
            "bad_value",
            format!("--seconds {seconds} is not a positive length"),
        )
        .with_path("--seconds"));
    }
    let mut preview = json!({
        "schema": RENDER_RIG_V1_SCHEMA,
        "sources": [RenderSource::Camera((*source).clone()).to_json()],
        "clip": { "startSeconds": at, "endSeconds": at + seconds },
    });
    if let Some(hosts) = &parsed.sensor_hosts {
        let host: Vec<&SensorHost> = hosts.iter().filter(|h| h.source_id == camera).collect();
        preview["sensorHosts"] = serde_json::to_value(host).expect("hosts serialize");
    }
    if let PreviewSpan::Clip { .. } = span {
        let a = &source.attributes;
        preview["video"] = json!({ "width": a.width, "height": a.height, "fps": fps, "container": "mp4", "codec": "h264", "quality": "standard" });
    }
    Rig::parse(&preview)?;
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn camera() -> Value {
        json!({
            "actorId": "ego", "sensorId": "dash-camera", "outputName": "ego-dash-camera-rgb", "modality": "rgb",
            "transform": { "position": { "x": 2, "y": 1.2, "z": 0 }, "rotation": { "yawRad": 0, "pitchRad": 0, "rollRad": 0 } },
            "attributes": { "width": 1280, "height": 720, "fps": 24, "horizontalFovDeg": 90, "nearM": 0.05, "farM": 1000 },
        })
    }

    fn lidar() -> Value {
        json!({
            "actorId": "ego", "sensorId": "roof-lidar", "outputName": "ego-roof-lidar-lidar", "modality": "lidar",
            "transform": { "position": { "x": 0, "y": 1.9, "z": 0 }, "rotation": { "yawRad": 0, "pitchRad": 0, "rollRad": 0.1 } },
            "attributes": { "channels": 64, "rangeM": 120, "pointsPerSecond": 1_200_000, "rotationFrequencyHz": 10, "upperFovDeg": 15, "lowerFovDeg": -15 },
        })
    }

    fn rig(sources: Vec<Value>) -> Rig {
        Rig::parse(&json!({ "schema": RENDER_RIG_V1_SCHEMA, "sources": sources })).unwrap()
    }

    #[test]
    fn rig_files_are_strict() {
        let mut bad = camera();
        bad["attributes"]["fsp"] = json!(24);
        assert!(Rig::parse(&json!({ "schema": RENDER_RIG_V1_SCHEMA, "sources": [bad] })).is_err());
        assert!(
            Rig::parse(&json!({ "schema": "simforge.render-rig/v9", "sources": [camera()] }))
                .is_err()
        );
        assert!(Rig::parse(
            &json!({ "schema": RENDER_RIG_V1_SCHEMA, "sources": [camera(), camera()] })
        )
        .is_err());
        let r = rig(vec![camera(), lidar()]);
        let (hosts, derived) = r
            .sensor_hosts(|id| (id == "ego").then(|| "vehicle.sedan".to_owned()))
            .unwrap();
        assert!(derived);
        assert_eq!(hosts[1].source_id, "ego-roof-lidar-lidar");
        assert_eq!(
            r.sensor_hosts(|_| None).unwrap_err().code,
            "native_sensor_host_unknown"
        );
        // The lidar horizontal FOV defaults to a full revolution, as the schema says.
        match &r.sources[1] {
            RenderSource::Lidar(s) => assert_eq!(s.attributes.horizontal_fov_deg, 360.0),
            _ => unreachable!(),
        }
        // Only a camera names its mount frame, and only the one it names is written back.
        let with = |mut source: Value, frame: &str| {
            source["mountFrame"] = json!(frame);
            Rig::parse(&json!({ "schema": RENDER_RIG_V1_SCHEMA, "sources": [source] }))
        };
        let gimbal = with(camera(), "gimbal").unwrap().sources[0].to_json();
        assert_eq!(gimbal["mountFrame"], "gimbal");
        assert_eq!(
            rig(vec![camera()]).sources[0].to_json().get("mountFrame"),
            None
        );
        assert!(with(lidar(), "gimbal").is_err() && with(camera(), "turret").is_err());
    }

    pub(crate) fn ftheta_camera() -> Value {
        let mut ftheta = camera();
        let attributes = ftheta["attributes"].as_object_mut().unwrap();
        attributes.remove("horizontalFovDeg");
        attributes.insert(
            "cameraModel".into(),
            json!({
                "type": "ftheta", "referenceResolution": [1920, 1080], "principalPoint": [959.5, 539.5],
                "pixeldistToAnglePoly": [0.0, 1.2e-3, 1.0e-8], "maxAngleRad": 1.2,
            }),
        );
        ftheta
    }

    #[test]
    fn an_ftheta_camera_is_checked_for_shape_only() {
        let parse = |source: &Value| {
            Rig::parse(&json!({ "schema": RENDER_RIG_V1_SCHEMA, "sources": [source] }))
        };
        let mut ftheta = ftheta_camera();
        let parsed = parse(&ftheta).unwrap();
        assert_eq!(
            parsed.sources[0].to_json()["attributes"]["cameraModel"],
            ftheta["attributes"]["cameraModel"]
        );
        assert!(parsed.has_lens());
        assert!(!rig(vec![camera()]).has_lens());
        ftheta["attributes"]["cameraModel"]["pixeldistToAnglePoly"] = json!([]);
        let error = parse(&ftheta).unwrap_err();
        assert_eq!(error.code, "render_rig_invalid");
        assert!(error.reason.contains("cameraModel"), "{}", error.reason);
        let mut unknown = ftheta_camera();
        unknown["attributes"]["cameraModel"]["type"] = json!("kannala");
        assert!(parse(&unknown).is_err(), "only f-theta");
        let mut both = ftheta_camera();
        both["attributes"]["horizontalFovDeg"] = json!(90);
        assert!(parse(&both).is_err(), "both lenses");
    }

    #[test]
    fn a_preview_keeps_one_camera_and_the_span() {
        let rig = json!({ "schema": RENDER_RIG_V1_SCHEMA, "sources": [camera(), lidar()] });
        let frame =
            preview_rig(&rig, "ego-dash-camera-rgb", PreviewSpan::Frame { at: 2.0 }).unwrap();
        let parsed = Rig::parse(&frame).unwrap();
        assert_eq!(parsed.sources.len(), 1);
        let clip = parsed.clip.unwrap();
        assert_eq!(clip.start_seconds, 2.0);
        assert!((clip.end_seconds - (2.0 + 1.0 / 24.0)).abs() < 1e-12);
        assert!(parsed.video.is_none());
        let clip = Rig::parse(
            &preview_rig(
                &rig,
                "ego-dash-camera-rgb",
                PreviewSpan::Clip {
                    at: 0.0,
                    seconds: 3.0,
                },
            )
            .unwrap(),
        )
        .unwrap();
        let video = clip.video.unwrap();
        assert_eq!((video.width, video.height, video.fps), (1280, 720, 24.0));
        let error =
            preview_rig(&rig, "ego-roof-lidar-lidar", PreviewSpan::Frame { at: 0.0 }).unwrap_err();
        assert_eq!(error.code, "preview_camera_unknown");
        assert_eq!(
            error.detail.unwrap()["cameras"],
            json!(["ego-dash-camera-rgb"])
        );
        assert_eq!(
            preview_rig(&rig, "ego-dash-camera-rgb", PreviewSpan::Frame { at: -1.0 })
                .unwrap_err()
                .code,
            "bad_value"
        );
    }

    #[test]
    fn a_preset_mounts_exactly_as_the_planner_fits_it() {
        // fitted/<preset>.<class>.rig.json is the TypeScript planner's presetRenderRig on the class's
        // default box with the training capture: the CLI's mount must be the same rig.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../catalog/sensor-rigs/fitted");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let (preset, class) = name.trim_end_matches(".rig.json").rsplit_once('.').unwrap();
            let fitted = Rig::parse(
                &serde_json::from_slice(&std::fs::read(dir.join(&name)).unwrap()).unwrap(),
            )
            .unwrap();
            let mounted = preset_rig(
                preset,
                "ego",
                default_box(class).unwrap(),
                profile_capture("training", class == "drone").unwrap(),
            )
            .unwrap();
            assert_eq!(mounted, fitted, "{name}");
            checked += 1;
        }
        assert_eq!(checked, 34);
        let showcase = profile_capture("showcase", false).unwrap();
        assert_eq!(
            (showcase.width, showcase.height, showcase.fps),
            (1280, 720, 24.0)
        );
        // A drone carrier captures 24 fps on Training; a road carrier keeps its 20.
        assert_eq!(profile_capture("training", true).unwrap().fps, 24.0);
        assert_eq!(profile_capture("training", false).unwrap().fps, 20.0);
        assert_eq!(
            preset_rig("nope", "ego", default_box("car").unwrap(), showcase)
                .unwrap_err()
                .code,
            "render_rig_preset_unknown"
        );
        assert_eq!(
            profile_capture("cinema", false).unwrap_err().code,
            "bad_value"
        );
    }
}
