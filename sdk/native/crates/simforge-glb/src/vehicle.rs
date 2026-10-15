//! The vehicle model contract: what a `.glb` must be for the native renderer
//! to draw it as a road vehicle (`docs/vehicle-models.md`;
//! `sdk/catalog/vehicles-carla/CONVENTIONS.md` is the shared catalog's
//! version of the same frame and graph).
//!
//! Every rule is a named finding. An `error` makes the model unusable; a
//! `warning` names a capability it lacks (static wheels, no paint tint, an
//! unnamed hull). The model is taken at its real size: there is no rescaling
//! to a catalog footprint.

use serde::Serialize;
use serde_json::{json, Value};

use crate::{image_size, transform, Glb};

/// Horizontal length/width ratio below which the length axis is ambiguous.
pub const MIN_ASPECT: f64 = 1.2;
/// Lowest point allowed this far from `y = 0` (tyre bottoms on the ground).
pub const GROUND_TOLERANCE_M: f64 = 0.05;
/// Horizontal centre allowed this fraction of the extent off the origin.
pub const CENTRE_TOLERANCE: f64 = 0.10;
/// Declared dimensions are matched within this fraction on every axis.
pub const DIMS_TOLERANCE: f64 = 0.10;
/// Triangles a vehicle model may draw.
pub const TRIANGLE_BUDGET: u64 = 400_000;
/// Largest texture side, pixels.
pub const TEXTURE_MAX_PX: u32 = 2048;
/// Extensions the renderer's glTF loader implements, so a model may require them.
pub const SUPPORTED_REQUIRED_EXTENSIONS: &[&str] = &[
    "KHR_lights_punctual",
    "KHR_materials_emissive_strength",
    "KHR_materials_ior",
    "KHR_materials_transmission",
    "KHR_materials_unlit",
    "KHR_materials_volume",
    "KHR_texture_transform",
];
/// The empty node an unrigged model places at its nose to say which end is
/// its front (`docs/asset-orientation.md`).
pub const FRONT_MARKER: &str = "front";
/// The four wheel nodes of a rigged four-wheeler, with the sign of their
/// position along +X (front) and +Z (right).
pub const WHEELS: [(&str, f64, f64); 4] = [
    ("wheel_fl", 1.0, -1.0),
    ("wheel_fr", 1.0, 1.0),
    ("wheel_rl", -1.0, -1.0),
    ("wheel_rr", -1.0, 1.0),
];

/// The road-vehicle classes a model can be validated as, with the plausible
/// footprint of each: `(length, width, height)` ranges in metres.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VehicleClass {
    Car,
    Van,
    Truck,
    Bus,
}

impl VehicleClass {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "car" => Some(Self::Car),
            "van" => Some(Self::Van),
            "truck" => Some(Self::Truck),
            "bus" => Some(Self::Bus),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Car => "car",
            Self::Van => "van",
            Self::Truck => "truck",
            Self::Bus => "bus",
        }
    }

    /// `[(min, max); 3]` for length, width and height, metres.
    pub const fn footprint(self) -> [(f64, f64); 3] {
        match self {
            // Widths include the mirrors.
            Self::Car => [(2.5, 6.5), (1.4, 2.6), (1.0, 2.4)],
            Self::Van => [(4.0, 8.0), (1.7, 2.9), (1.7, 3.4)],
            Self::Truck => [(4.5, 21.0), (1.9, 3.6), (2.0, 4.5)],
            Self::Bus => [(6.0, 19.0), (2.0, 4.0), (2.5, 4.5)],
        }
    }
}

/// What the model is checked against.
#[derive(Debug, Clone, Copy)]
pub struct Expect {
    pub class: VehicleClass,
    /// Declared `[length, width, height]`, matched within [`DIMS_TOLERANCE`].
    pub dims: Option<[f64; 3]>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

/// The validation result: the measured model and every finding.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub schema: &'static str,
    pub ok: bool,
    pub class: VehicleClass,
    pub sha256: String,
    pub bytes: u64,
    /// `[length (X), width (Z), height (Y)]`, metres.
    pub dims: Option<[f64; 3]>,
    pub triangles: u64,
    pub wheels: bool,
    /// What says which end is the front: the rigged wheels (`wheels`) or the
    /// `front` marker node (`marker`); absent when nothing does, which is an
    /// error (`vehicle_front_undeclared`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub front: Option<FrontEvidence>,
    pub tintable: bool,
    pub findings: Vec<Finding>,
}

/// The declaration a model's front is taken from. A footprint is the same both
/// ways round, so geometry alone cannot tell a front from a rear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontEvidence {
    Wheels,
    Marker,
}

pub const REPORT_SCHEMA: &str = "simforge.vehicle-validation/v1";

struct Out(Vec<Finding>);

impl Out {
    fn error(&mut self, code: &'static str, message: String, detail: Value) {
        self.0.push(Finding {
            code,
            severity: Severity::Error,
            message,
            detail,
        });
    }
    fn warning(&mut self, code: &'static str, message: String, detail: Value) {
        self.0.push(Finding {
            code,
            severity: Severity::Warning,
            message,
            detail,
        });
    }
}

/// Validate a read GLB against the vehicle model contract.
pub fn validate(glb: &Glb, expect: Expect) -> Report {
    let mut out = Out(Vec::new());
    let gltf = &glb.json;

    for ext in gltf["extensionsRequired"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !SUPPORTED_REQUIRED_EXTENSIONS.contains(&ext) {
            out.error(
                "vehicle_extension_unsupported",
                format!("the model requires glTF extension {ext}, which the renderer does not implement (supported: {})", SUPPORTED_REQUIRED_EXTENSIONS.join(", ")),
                json!({ "extension": ext }),
            );
        }
    }

    let nodes = glb.scene_nodes();
    for node in &nodes {
        if node.local_scale.iter().any(|s| *s < 0.0) {
            out.error(
                "vehicle_negative_scale",
                format!("node {} has a negative scale {:?}; apply it to the mesh (mirrored geometry renders inside out)", node_label(node.name.as_deref(), node.index), node.local_scale),
                json!({ "node": node.name, "scale": node.local_scale }),
            );
        }
    }

    let bounds = glb.bounds();
    let mut dims = None;
    match &bounds {
        Err(reason) => out.error(
            "vehicle_geometry_missing",
            format!("the model cannot be measured: {reason}"),
            Value::Null,
        ),
        Ok((lo, hi)) => {
            let ext: [f64; 3] = std::array::from_fn(|a| hi[a] - lo[a]);
            let (x, y, z) = (ext[0], ext[1], ext[2]);
            dims = Some([x, z, y]);
            if x < MIN_ASPECT * z {
                out.error(
                    "vehicle_misoriented",
                    format!("its length must run along +X (forward) and be at least {MIN_ASPECT}x its width along Z, but X is {x:.3} m and Z {z:.3} m{}", if z > x { "; rotate it 90 degrees about +Y" } else { "" }),
                    json!({ "extentX": x, "extentZ": z }),
                );
            }
            if lo[1].abs() > GROUND_TOLERANCE_M {
                out.error(
                    "vehicle_not_grounded",
                    format!("its lowest point is at y = {:.3} m; the tyre bottoms must sit at y = 0 (within {GROUND_TOLERANCE_M} m), with +Y up", lo[1]),
                    json!({ "minY": lo[1] }),
                );
            }
            for (axis, name, extent) in [(0usize, "X", x), (2, "Z", z)] {
                let centre = (lo[axis] + hi[axis]) / 2.0;
                if centre.abs() > CENTRE_TOLERANCE * extent {
                    out.error(
                        "vehicle_off_centre",
                        format!("its {name} centre is {centre:.3} m from the origin; the origin must be the footprint centre (within {:.0}% of its {name} extent)", CENTRE_TOLERANCE * 100.0),
                        json!({ "axis": name, "centre": centre, "extent": extent }),
                    );
                }
            }
            let measured = [x, z, y];
            let names = ["length", "width", "height"];
            match expect.dims {
                Some(declared) => {
                    for i in 0..3 {
                        if (measured[i] - declared[i]).abs() > DIMS_TOLERANCE * declared[i] {
                            out.error(
                                "vehicle_dims_mismatch",
                                format!("its {} is {:.3} m but {:.3} m was declared (within {:.0}%)", names[i], measured[i], declared[i], DIMS_TOLERANCE * 100.0),
                                json!({ "axis": names[i], "measured": measured[i], "declared": declared[i] }),
                            );
                        }
                    }
                }
                None => {
                    for (i, (min, max)) in expect.class.footprint().into_iter().enumerate() {
                        if measured[i] < min || measured[i] > max {
                            out.error(
                                "vehicle_dims_implausible",
                                format!("its {} is {:.3} m, outside {min}-{max} m for a {} (is it in metres? declare --dims to state the real size)", names[i], measured[i], expect.class.as_str()),
                                json!({ "axis": names[i], "measured": measured[i], "range": [min, max], "class": expect.class.as_str() }),
                            );
                        }
                    }
                }
            }
        }
    }

    let named = |name: &str| nodes.iter().find(|n| n.name.as_deref() == Some(name));
    if named("body").is_none() {
        out.warning(
            "vehicle_body_unnamed",
            "no node is named `body` (the hull, interior, fixed glass and lights): the model renders, but tools that address the hull cannot find it".into(),
            Value::Null,
        );
    }
    let present: Vec<&str> = WHEELS
        .iter()
        .map(|w| w.0)
        .filter(|w| named(w).is_some())
        .collect();
    let wheels = present.len() == WHEELS.len();
    let mut front = None;
    if present.is_empty() {
        out.warning(
            "vehicle_wheels_static",
            "no wheel_fl/wheel_fr/wheel_rl/wheel_rr nodes: the wheels will not turn or spin".into(),
            Value::Null,
        );
        match named(FRONT_MARKER) {
            Some(marker) => {
                let p = transform(&marker.world, [0.0, 0.0, 0.0]);
                if p[0] > p[2].abs() {
                    front = Some(FrontEvidence::Marker);
                } else if -p[0] > p[2].abs() {
                    out.error(
                        "vehicle_reversed",
                        format!("its `{FRONT_MARKER}` marker is at x = {:.3}: the model faces backwards; turn it 180 degrees about +Y", p[0]),
                        json!({ "evidence": "marker", "marker": p }),
                    );
                } else {
                    out.error(
                        "vehicle_front_misplaced",
                        format!("its `{FRONT_MARKER}` marker is at x = {:.3}, z = {:.3}: the front must be at the +X end of the model", p[0], p[2]),
                        json!({ "marker": p }),
                    );
                }
            }
            None => out.error(
                "vehicle_front_undeclared",
                format!("nothing says which end is the front: an outline is the same both ways round. Rig the four wheel nodes (wheel_fl and wheel_fr at +X) or add an empty node named `{FRONT_MARKER}` at the nose (+X)"),
                Value::Null,
            ),
        }
    } else if !wheels {
        let missing: Vec<&str> = WHEELS
            .iter()
            .map(|w| w.0)
            .filter(|w| !present.contains(w))
            .collect();
        out.error(
            "vehicle_wheels_incomplete",
            format!(
                "it has {} but not {}: a rigged vehicle names all four wheel nodes",
                present.join(", "),
                missing.join(", ")
            ),
            json!({ "present": present, "missing": missing }),
        );
    } else {
        let pivots: Vec<[f64; 3]> = WHEELS
            .iter()
            .map(|(name, _, _)| transform(&named(name).expect("present").world, [0.0, 0.0, 0.0]))
            .collect();
        // Every wheel where a 180 degree turn about +Y would put it: the model faces -X.
        let turned = WHEELS
            .iter()
            .zip(&pivots)
            .all(|((_, front, right), p)| p[0].signum() == -front && p[2].signum() == -right);
        if turned {
            out.error(
                "vehicle_reversed",
                "its front wheels (wheel_fl, wheel_fr) are at -X: the model faces backwards; turn it 180 degrees about +Y".into(),
                json!({ "evidence": "wheels" }),
            );
        } else {
            front = Some(FrontEvidence::Wheels);
            for ((name, front, right), p) in WHEELS.iter().zip(&pivots) {
                if p[0].signum() != *front || p[2].signum() != *right {
                    out.error(
                        "vehicle_wheel_misplaced",
                        format!("{name}'s pivot is at x = {:.3}, z = {:.3}; {} wheels sit at {} x and {} wheels at {} z (+X forward, +Z right), with the pivot at the wheel centre", p[0], p[2], if *front > 0.0 { "front" } else { "rear" }, if *front > 0.0 { "+" } else { "-" }, if *right > 0.0 { "right" } else { "left" }, if *right > 0.0 { "+" } else { "-" }),
                        json!({ "node": name, "pivot": p }),
                    );
                }
            }
        }
    }

    let triangles = glb.triangles();
    if triangles > TRIANGLE_BUDGET {
        out.error(
            "vehicle_over_budget",
            format!("it draws {triangles} triangles; a vehicle may draw at most {TRIANGLE_BUDGET} (decimate it)"),
            json!({ "triangles": triangles, "budget": TRIANGLE_BUDGET }),
        );
    }

    let mut textured = std::collections::BTreeSet::new();
    for (i, material) in gltf["materials"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let pbr = &material["pbrMetallicRoughness"];
        if pbr["baseColorTexture"].is_object() || material["normalTexture"].is_object() {
            textured.insert(i as u64);
        }
    }
    let mut missing_attrs = std::collections::BTreeMap::<&'static str, usize>::new();
    for mesh in gltf["meshes"].as_array().into_iter().flatten() {
        for primitive in mesh["primitives"].as_array().into_iter().flatten() {
            let attrs = &primitive["attributes"];
            if !attrs["NORMAL"].is_u64() {
                *missing_attrs.entry("NORMAL").or_default() += 1;
            }
            let needs_uv = primitive["material"]
                .as_u64()
                .is_some_and(|m| textured.contains(&m));
            if needs_uv && !attrs["TEXCOORD_0"].is_u64() {
                *missing_attrs.entry("TEXCOORD_0").or_default() += 1;
            }
        }
    }
    for (attribute, count) in missing_attrs {
        out.error(
            "vehicle_attribute_missing",
            format!(
                "{count} primitive(s) have no {attribute}{}",
                if attribute == "NORMAL" {
                    " (export normals)"
                } else {
                    " but use a textured material"
                }
            ),
            json!({ "attribute": attribute, "primitives": count }),
        );
    }

    for (i, image) in gltf["images"].as_array().into_iter().flatten().enumerate() {
        let label = image["name"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("image {i}"));
        if image["uri"].is_string() {
            out.error(
                "vehicle_texture_external",
                format!(
                    "{label} is an external file; a .glb carries every texture in its binary chunk"
                ),
                json!({ "image": i }),
            );
            continue;
        }
        let mime = image["mimeType"].as_str().unwrap_or("");
        if mime != "image/png" && mime != "image/jpeg" {
            out.error(
                "vehicle_texture_unsupported",
                format!("{label} is {mime:?}; textures are PNG or JPEG"),
                json!({ "image": i, "mimeType": mime }),
            );
            continue;
        }
        match glb.image_bytes(i).and_then(image_size) {
            Some((w, h)) if w.max(h) > TEXTURE_MAX_PX => out.error(
                "vehicle_texture_too_large",
                format!("{label} is {w}x{h}; a texture side is at most {TEXTURE_MAX_PX} px"),
                json!({ "image": i, "width": w, "height": h }),
            ),
            Some(_) => {}
            None => out.error(
                "vehicle_texture_unreadable",
                format!("{label}'s {mime} bytes cannot be read"),
                json!({ "image": i }),
            ),
        }
    }

    let tintable = gltf["materials"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|m| m["name"].as_str() == Some("body_paint"));
    if !tintable {
        out.warning(
            "vehicle_not_tintable",
            "no material is named `body_paint`: scenarios cannot recolour the paint".into(),
            Value::Null,
        );
    }

    let ok = !out.0.iter().any(|f| f.severity == Severity::Error);
    Report {
        schema: REPORT_SCHEMA,
        ok,
        class: expect.class,
        sha256: glb.sha256.clone(),
        bytes: glb.bytes,
        dims,
        triangles,
        wheels,
        front,
        tintable,
        findings: out.0,
    }
}

fn node_label(name: Option<&str>, index: usize) -> String {
    name.map(|n| format!("`{n}`"))
        .unwrap_or_else(|| format!("#{index}"))
}
