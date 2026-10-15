//! Binary glTF (GLB) for SimForge actor models: reading a `.glb`
//! ([`Glb::read`]), measuring its default scene ([`Glb::extents`],
//! [`Glb::triangles`]) and the vehicle model contract `simforge vehicle
//! validate` checks ([`vehicle`]).
//!
//! Only the container and its JSON are interpreted; mesh bytes are never
//! decoded. Bounds come from each `POSITION` accessor's required `min`/`max`
//! through the node transforms, triangle counts from accessor counts.

pub mod vehicle;

use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

const GLB_MAGIC: &[u8; 4] = b"glTF";
const CHUNK_JSON: u32 = 0x4e4f_534a;
const CHUNK_BIN: u32 = 0x004e_4942;

/// A read GLB: its JSON, its binary chunk and the file's digest.
#[derive(Debug, Clone)]
pub struct Glb {
    pub json: Value,
    pub bin: Vec<u8>,
    pub sha256: String,
    pub bytes: u64,
}

/// A node of the default scene with its world transform.
#[derive(Debug, Clone)]
pub struct SceneNode {
    pub index: usize,
    pub name: Option<String>,
    /// Column-major 4x4 world matrix.
    pub world: [f64; 16],
    /// The node's own (local) scale as authored, `[1, 1, 1]` when absent.
    pub local_scale: [f64; 3],
}

const IDENTITY: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

impl Glb {
    /// Read a `.glb` file: a glTF 2.0 binary container with a JSON chunk and
    /// an optional binary chunk. `Err` names what is wrong with the container.
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let word = |i: usize| -> Option<u32> {
            bytes
                .get(i..i + 4)
                .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")))
        };
        if bytes.len() < 20 || &bytes[..4] != GLB_MAGIC {
            return Err("not a binary glTF (no glTF header)".into());
        }
        if word(4) != Some(2) {
            return Err(format!(
                "glTF container version {} (only 2 is supported)",
                word(4).unwrap_or(0)
            ));
        }
        let mut at = 12usize;
        let (mut json, mut bin) = (None, Vec::new());
        while at + 8 <= bytes.len() {
            let (length, kind) = (word(at).unwrap_or(0) as usize, word(at + 4).unwrap_or(0));
            let body = bytes
                .get(at + 8..at + 8 + length)
                .ok_or_else(|| format!("chunk at byte {at} runs past the end of the file"))?;
            match kind {
                CHUNK_JSON if json.is_none() => {
                    json = Some(
                        serde_json::from_slice::<Value>(body)
                            .map_err(|e| format!("its JSON chunk is not JSON: {e}"))?,
                    )
                }
                CHUNK_BIN if bin.is_empty() => bin = body.to_vec(),
                _ => {}
            }
            at += 8 + length;
        }
        let json = json.ok_or("no JSON chunk")?;
        if json["asset"]["version"].as_str() != Some("2.0") {
            return Err(format!(
                "asset.version {} (only \"2.0\" is supported)",
                json["asset"]["version"]
            ));
        }
        Ok(Self {
            json,
            bin,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            bytes: bytes.len() as u64,
        })
    }

    /// Every node of the default scene, depth first, with its world transform.
    pub fn scene_nodes(&self) -> Vec<SceneNode> {
        let gltf = &self.json;
        let scene = &gltf["scenes"][gltf["scene"].as_u64().unwrap_or(0) as usize];
        let mut out = Vec::new();
        let mut stack: Vec<(u64, [f64; 16])> = scene["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|n| (n, IDENTITY))
            .rev()
            .collect();
        while let Some((index, parent)) = stack.pop() {
            let node = &gltf["nodes"][index as usize];
            let world = mat_mul(&parent, &node_matrix(node));
            let s = |i: usize| node["scale"][i].as_f64().unwrap_or(1.0);
            out.push(SceneNode {
                index: index as usize,
                name: node["name"].as_str().map(str::to_owned),
                world,
                local_scale: [s(0), s(1), s(2)],
            });
            for child in node["children"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .rev()
            {
                stack.push((child, world));
            }
        }
        out
    }

    /// Scene-space bounds `(min, max)` of the default scene: each `POSITION`
    /// accessor's min/max box through its node transforms.
    pub fn bounds(&self) -> Result<([f64; 3], [f64; 3]), String> {
        let gltf = &self.json;
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for node in self.scene_nodes() {
            let Some(mesh) = gltf["nodes"][node.index]["mesh"].as_u64() else {
                continue;
            };
            for primitive in gltf["meshes"][mesh as usize]["primitives"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let accessor = primitive["attributes"]["POSITION"]
                    .as_u64()
                    .map(|a| &gltf["accessors"][a as usize])
                    .ok_or_else(|| format!("mesh {mesh} has a primitive without POSITION"))?;
                let bound = |key: &str| -> Result<[f64; 3], String> {
                    let b = |i: usize| accessor[key][i].as_f64();
                    match (b(0), b(1), b(2)) {
                        (Some(x), Some(y), Some(z)) => Ok([x, y, z]),
                        _ => Err(format!("mesh {mesh} POSITION has no {key}")),
                    }
                };
                let (min, max) = (bound("min")?, bound("max")?);
                for corner in 0..8 {
                    let p: [f64; 3] = std::array::from_fn(|axis| {
                        if corner >> axis & 1 == 1 {
                            max[axis]
                        } else {
                            min[axis]
                        }
                    });
                    let w = transform(&node.world, p);
                    for axis in 0..3 {
                        lo[axis] = lo[axis].min(w[axis]);
                        hi[axis] = hi[axis].max(w[axis]);
                    }
                }
            }
        }
        if !lo.iter().chain(&hi).all(|v| v.is_finite()) {
            return Err("its scene has no mesh geometry".into());
        }
        Ok((lo, hi))
    }

    /// Scene-space extents `[x, y, z]` of the default scene.
    pub fn extents(&self) -> Result<[f64; 3], String> {
        let (lo, hi) = self.bounds()?;
        Ok(std::array::from_fn(|axis| hi[axis] - lo[axis]))
    }

    /// Triangles the default scene draws: every triangle-list primitive of
    /// every mesh instance (index count, else vertex count, over three).
    pub fn triangles(&self) -> u64 {
        let gltf = &self.json;
        self.scene_nodes()
            .iter()
            .filter_map(|node| gltf["nodes"][node.index]["mesh"].as_u64())
            .flat_map(|mesh| {
                gltf["meshes"][mesh as usize]["primitives"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|p| p["mode"].as_u64().unwrap_or(4) == 4)
            .map(|p| {
                let count = |a: &Value| {
                    a.as_u64()
                        .map(|i| gltf["accessors"][i as usize]["count"].as_u64().unwrap_or(0))
                };
                count(&p["indices"])
                    .or_else(|| count(&p["attributes"]["POSITION"]))
                    .unwrap_or(0)
                    / 3
            })
            .sum()
    }

    /// The bytes of an image (`images[index]`), from its buffer view.
    pub fn image_bytes(&self, index: usize) -> Option<&[u8]> {
        let view =
            &self.json["bufferViews"][self.json["images"][index]["bufferView"].as_u64()? as usize];
        let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize;
        let length = view["byteLength"].as_u64()? as usize;
        self.bin.get(offset..offset + length)
    }
}

/// Pixel size `(width, height)` of a PNG or baseline/progressive JPEG.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let be = |i: usize| u32::from_be_bytes(bytes[i..i + 4].try_into().expect("4 bytes"));
        return Some((be(16), be(20)));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        let mut at = 2usize;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xff {
                return None;
            }
            let marker = bytes[at + 1];
            let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                let h = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]) as u32;
                return Some((w, h));
            }
            at += 2 + length;
        }
    }
    None
}

fn node_matrix(node: &Value) -> [f64; 16] {
    let v = |key: &str, i: usize, default: f64| node[key][i].as_f64().unwrap_or(default);
    if node["matrix"].is_array() {
        return std::array::from_fn(|i| v("matrix", i, 0.0));
    }
    let (x, y, z, w) = (
        v("rotation", 0, 0.0),
        v("rotation", 1, 0.0),
        v("rotation", 2, 0.0),
        v("rotation", 3, 1.0),
    );
    let (sx, sy, sz) = (v("scale", 0, 1.0), v("scale", 1, 1.0), v("scale", 2, 1.0));
    [
        (1.0 - 2.0 * (y * y + z * z)) * sx,
        2.0 * (x * y + z * w) * sx,
        2.0 * (x * z - y * w) * sx,
        0.0,
        2.0 * (x * y - z * w) * sy,
        (1.0 - 2.0 * (x * x + z * z)) * sy,
        2.0 * (y * z + x * w) * sy,
        0.0,
        2.0 * (x * z + y * w) * sz,
        2.0 * (y * z - x * w) * sz,
        (1.0 - 2.0 * (x * x + y * y)) * sz,
        0.0,
        v("translation", 0, 0.0),
        v("translation", 1, 0.0),
        v("translation", 2, 0.0),
        1.0,
    ]
}

fn mat_mul(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    std::array::from_fn(|i| {
        let (column, row) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum()
    })
}

/// A point through a column-major world matrix.
pub fn transform(m: &[f64; 16], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| {
        m[axis] * p[0] + m[4 + axis] * p[1] + m[8 + axis] * p[2] + m[12 + axis]
    })
}
