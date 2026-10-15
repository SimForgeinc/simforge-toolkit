//! The vehicle model contract on synthetic GLBs: a well-formed car passes,
//! and each broken rule is named.

use serde_json::{json, Value};
use simforge_glb::vehicle::{validate, Expect, FrontEvidence, Severity, VehicleClass};
use simforge_glb::Glb;

/// A GLB whose scene is a `body` box `(l, h, w)` with its bottom at `ground`
/// and four wheel nodes, unless `edit` changes the JSON.
fn car(l: f64, w: f64, h: f64, ground: f64, edit: impl FnOnce(&mut Value)) -> Glb {
    let mut gltf = car_json(l, w, h, ground);
    edit(&mut gltf);
    Glb::parse(&container(&gltf, &[])).expect("a valid container")
}

fn car_json(l: f64, w: f64, h: f64, ground: f64) -> Value {
    json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [
            { "name": "root", "children": [1, 2, 3, 4, 5] },
            { "name": "body", "mesh": 0 },
            { "name": "wheel_fl", "translation": [l / 3.0, 0.35, -w / 2.5] },
            { "name": "wheel_fr", "translation": [l / 3.0, 0.35, w / 2.5] },
            { "name": "wheel_rl", "translation": [-l / 3.0, 0.35, -w / 2.5] },
            { "name": "wheel_rr", "translation": [-l / 3.0, 0.35, w / 2.5] }
        ],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0, "NORMAL": 1 }, "indices": 2, "material": 0 }] }],
        "materials": [{ "name": "body_paint", "pbrMetallicRoughness": { "baseColorFactor": [1, 1, 1, 1] } }],
        "accessors": [
            { "count": 8, "type": "VEC3", "componentType": 5126,
              "min": [-l / 2.0, ground, -w / 2.0], "max": [l / 2.0, ground + h, w / 2.0] },
            { "count": 8, "type": "VEC3", "componentType": 5126 },
            { "count": 36, "type": "SCALAR", "componentType": 5123 }
        ]
    })
}

fn container(gltf: &Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(gltf).unwrap();
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    let mut bin = bin.to_vec();
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::new();
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4e4f_534au32.to_le_bytes());
    out.extend_from_slice(&json);
    if !bin.is_empty() {
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004e_4942u32.to_le_bytes());
        out.extend_from_slice(&bin);
    }
    out
}

const CAR: Expect = Expect {
    class: VehicleClass::Car,
    dims: None,
};

fn errors(glb: &Glb, expect: Expect) -> Vec<&'static str> {
    let report = validate(glb, expect);
    assert_eq!(
        report.ok,
        report
            .findings
            .iter()
            .all(|f| f.severity != Severity::Error)
    );
    report
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .map(|f| f.code)
        .collect()
}

#[test]
fn a_well_formed_car_passes_with_its_measurements() {
    let glb = car(4.6, 1.9, 1.5, 0.0, |_| {});
    let report = validate(&glb, CAR);
    assert!(report.ok, "{:?}", report.findings);
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    let [l, w, h] = report.dims.unwrap();
    assert!((l - 4.6).abs() < 1e-9 && (w - 1.9).abs() < 1e-9 && (h - 1.5).abs() < 1e-9);
    assert_eq!(report.triangles, 12);
    assert!(report.wheels && report.tintable);
    assert_eq!(report.front, Some(FrontEvidence::Wheels));
    let declared = Expect {
        class: VehicleClass::Car,
        dims: Some([4.5, 1.85, 1.45]),
    };
    assert!(validate(&glb, declared).ok);
}

#[test]
fn each_broken_rule_is_named() {
    // Length along Z: the model faces sideways.
    assert_eq!(
        errors(&car(1.9, 4.6, 1.5, 0.0, |_| {}), CAR)[0],
        "vehicle_misoriented"
    );
    // Floating 0.3 m above the ground.
    assert_eq!(
        errors(&car(4.6, 1.9, 1.5, 0.3, |_| {}), CAR),
        ["vehicle_not_grounded"]
    );
    // Origin at the rear bumper.
    let rear = car(4.6, 1.9, 1.5, 0.0, |g| {
        g["nodes"][1]["translation"] = json!([2.3, 0.0, 0.0])
    });
    assert!(errors(&rear, CAR).contains(&"vehicle_off_centre"));
    // In centimetres.
    assert!(
        errors(&car(460.0, 190.0, 150.0, 0.0, |_| {}), CAR).contains(&"vehicle_dims_implausible")
    );
    // Declared size it does not have.
    let declared = Expect {
        class: VehicleClass::Car,
        dims: Some([5.5, 1.9, 1.5]),
    };
    assert_eq!(
        errors(&car(4.6, 1.9, 1.5, 0.0, |_| {}), declared),
        ["vehicle_dims_mismatch"]
    );
    // Three of four wheels.
    let three = car(4.6, 1.9, 1.5, 0.0, |g| g["nodes"][5]["name"] = json!("hub"));
    assert_eq!(errors(&three, CAR), ["vehicle_wheels_incomplete"]);
    // Front-left wheel behind the axle line.
    let swapped = car(4.6, 1.9, 1.5, 0.0, |g| {
        g["nodes"][2]["translation"] = json!([-1.5, 0.35, -0.76])
    });
    assert_eq!(errors(&swapped, CAR), ["vehicle_wheel_misplaced"]);
    // A mirrored node.
    let mirrored = car(4.6, 1.9, 1.5, 0.0, |g| {
        g["nodes"][1]["scale"] = json!([1.0, 1.0, -1.0])
    });
    assert!(errors(&mirrored, CAR).contains(&"vehicle_negative_scale"));
    // Draco compression the renderer cannot decode.
    let draco = car(4.6, 1.9, 1.5, 0.0, |g| {
        g["extensionsRequired"] = json!(["KHR_draco_mesh_compression"])
    });
    assert_eq!(errors(&draco, CAR), ["vehicle_extension_unsupported"]);
    // Over the triangle budget.
    let heavy = car(4.6, 1.9, 1.5, 0.0, |g| {
        g["accessors"][2]["count"] = json!(3 * 400_001)
    });
    assert_eq!(errors(&heavy, CAR), ["vehicle_over_budget"]);
    // No normals.
    let flat = car(4.6, 1.9, 1.5, 0.0, |g| {
        g["meshes"][0]["primitives"][0]["attributes"]
            .as_object_mut()
            .unwrap()
            .remove("NORMAL");
    });
    assert_eq!(errors(&flat, CAR), ["vehicle_attribute_missing"]);
    // An external texture file.
    let external = car(4.6, 1.9, 1.5, 0.0, |g| {
        g["images"] = json!([{ "uri": "paint.png" }])
    });
    assert_eq!(errors(&external, CAR), ["vehicle_texture_external"]);
}

#[test]
fn textures_are_png_or_jpeg_within_the_size_cap() {
    let png = |w: u32, h: u32| {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    };
    let with_image = |bytes: Vec<u8>, mime: &str| {
        let mut gltf = car_json(4.6, 1.9, 1.5, 0.0);
        gltf["bufferViews"] = json!([{ "buffer": 0, "byteOffset": 0, "byteLength": bytes.len() }]);
        gltf["buffers"] = json!([{ "byteLength": bytes.len() }]);
        gltf["images"] = json!([{ "bufferView": 0, "mimeType": mime }]);
        Glb::parse(&container(&gltf, &bytes)).unwrap()
    };
    assert!(errors(&with_image(png(1024, 1024), "image/png"), CAR).is_empty());
    assert_eq!(
        errors(&with_image(png(4096, 2048), "image/png"), CAR),
        ["vehicle_texture_too_large"]
    );
    assert_eq!(
        errors(&with_image(png(64, 64), "image/webp"), CAR),
        ["vehicle_texture_unsupported"]
    );
}

/// The car without wheel nodes, with an empty `front` node at `front` (or none).
fn unrigged(front: Option<[f64; 3]>) -> Glb {
    car(4.6, 1.9, 1.5, 0.0, |g| {
        g["nodes"][0]["children"] = json!([1]);
        g["materials"][0]["name"] = json!("paint");
        if let Some(at) = front {
            g["nodes"][2] = json!({ "name": "front", "translation": at });
            g["nodes"][0]["children"] = json!([1, 2]);
        }
    })
}

#[test]
fn a_static_untinted_model_with_a_front_marker_passes_with_warnings() {
    let report = validate(&unrigged(Some([2.3, 0.75, 0.0])), CAR);
    assert!(report.ok, "{:?}", report.findings);
    assert_eq!(report.front, Some(FrontEvidence::Marker));
    let warnings: Vec<_> = report.findings.iter().map(|f| f.code).collect();
    assert_eq!(warnings, ["vehicle_wheels_static", "vehicle_not_tintable"]);
}

#[test]
fn a_model_facing_backwards_is_refused() {
    // Its outline is the same both ways round: without a declared front it
    // cannot pass, whichever way it faces.
    let undeclared = validate(&unrigged(None), CAR);
    assert_eq!(undeclared.front, None);
    assert_eq!(errors(&unrigged(None), CAR), ["vehicle_front_undeclared"]);
    // The nose marker behind the origin: turned 180 degrees.
    assert_eq!(
        errors(&unrigged(Some([-2.3, 0.75, 0.0])), CAR),
        ["vehicle_reversed"]
    );
    // The nose marker off to a side.
    assert_eq!(
        errors(&unrigged(Some([0.2, 0.75, 0.9])), CAR),
        ["vehicle_front_misplaced"]
    );
    // Every wheel where a 180 degree turn puts it: front pair at -X, left at +Z.
    let turned = car(4.6, 1.9, 1.5, 0.0, |g| {
        for node in 2..6 {
            let at = g["nodes"][node]["translation"].clone();
            g["nodes"][node]["translation"] =
                json!([-at[0].as_f64().unwrap(), at[1], -at[2].as_f64().unwrap()]);
        }
    });
    let report = validate(&turned, CAR);
    assert_eq!(report.front, None);
    assert_eq!(errors(&turned, CAR), ["vehicle_reversed"]);
}

#[test]
fn a_file_that_is_not_a_glb_is_refused_by_name() {
    assert!(Glb::parse(b"{\"asset\":{}}")
        .unwrap_err()
        .contains("not a binary glTF"));
    let v1 = json!({ "asset": { "version": "1.0" } });
    assert!(Glb::parse(&container(&v1, &[]))
        .unwrap_err()
        .contains("asset.version"));
}
