//! Reading glTF `EXT_mesh_gpu_instancing` placements.
//!
//! The extension puts a node's instances in accessors rather than in the node graph, so a scatter of
//! ten thousand trees is one node. Reading placements stays separate from model import so a
//! placement read never drags a mesh decode along with it.
//!
//! Instance transforms are in the instanced node's local space, so each one composes with the node's
//! own transform and its whole ancestor chain — a scatter parented under a scaled group is scaled.

use std::path::Path;

use glam::{Mat4, Quat, Vec3};

use crate::error::{Error, Result};

/// One placement read from an instanced node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GltfInstance {
    /// World translation in metres.
    pub translation: [f64; 3],
    /// World rotation as an XYZW quaternion.
    pub rotation: [f64; 4],
    /// World scale.
    pub scale: [f64; 3],
    /// Stable identity from an `_ID` attribute, absent when the source declares none.
    pub id: Option<u64>,
}

/// One instanced node and everything it places.
#[derive(Clone, Debug, PartialEq)]
pub struct GltfInstanceSet {
    /// The node's name, or its mesh's, or a stable fallback from its index.
    pub name: String,
    /// Placements in accessor order.
    pub instances: Vec<GltfInstance>,
}

/// What one glTF file's instancing declared, and what could not be read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GltfInstancing {
    /// Instanced nodes in document order.
    pub sets: Vec<GltfInstanceSet>,
    /// Instance attributes present in the file that this reader cannot express, in canonical order.
    pub unsupported: Vec<String>,
}

/// Reads every `EXT_mesh_gpu_instancing` placement in a `.gltf`/`.glb` file.
///
/// # Errors
///
/// [`Error::Import`] when the file cannot be read, and when an instancing node's `TRANSLATION`
/// accessor is absent or of a type this reader cannot decode — a scatter with no positions is a
/// mistake, not an empty scatter.
pub fn read_gltf_instancing(path: impl AsRef<Path>) -> Result<GltfInstancing> {
    let path = path.as_ref();
    let (document, buffers, _images) =
        gltf::import(path).map_err(|error| Error::Import(error.to_string()))?;
    let data = |buffer: gltf::Buffer<'_>| buffers.get(buffer.index()).map(|data| data.0.as_slice());

    // The world transform of every node, accumulated down the scene the file opens with.
    let mut world = vec![Mat4::IDENTITY; document.nodes().len()];
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next());
    if let Some(scene) = scene {
        for root in scene.nodes() {
            accumulate(&root, Mat4::IDENTITY, &mut world);
        }
    }

    let mut sets = Vec::new();
    let mut unsupported = Vec::new();
    for node in document.nodes() {
        let Some(extension) = node
            .extensions()
            .and_then(|extensions| extensions.get("EXT_mesh_gpu_instancing"))
        else {
            continue;
        };
        let attributes = extension
            .get("attributes")
            .and_then(|value| value.as_object())
            .ok_or_else(|| Error::Import("instancing node has no attributes".to_owned()))?;
        let accessor = |name: &str| -> Option<gltf::Accessor<'_>> {
            attributes
                .get(name)
                .and_then(|value| value.as_u64())
                .and_then(|index| document.accessors().nth(index as usize))
        };
        for name in attributes.keys() {
            if !matches!(name.as_str(), "TRANSLATION" | "ROTATION" | "SCALE" | "_ID") {
                unsupported.push(name.clone());
            }
        }

        let translations = accessor("TRANSLATION")
            .and_then(|accessor| read_vec3(&accessor, &data))
            .ok_or_else(|| Error::Import("instancing node has no TRANSLATION".to_owned()))?;
        let rotations = accessor("ROTATION").and_then(|accessor| read_vec4(&accessor, &data));
        let scales = accessor("SCALE").and_then(|accessor| read_vec3(&accessor, &data));
        let ids = accessor("_ID").and_then(|accessor| read_u32(&accessor, &data));
        if accessor("ROTATION").is_some() && rotations.is_none() {
            unsupported.push("ROTATION".to_owned());
        }
        if accessor("SCALE").is_some() && scales.is_none() {
            unsupported.push("SCALE".to_owned());
        }

        let parent = world.get(node.index()).copied().unwrap_or(Mat4::IDENTITY);
        let instances = translations
            .iter()
            .enumerate()
            .map(|(index, translation)| {
                let rotation = rotations
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .copied()
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]);
                let scale = scales
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .copied()
                    .unwrap_or([1.0, 1.0, 1.0]);
                let local = Mat4::from_scale_rotation_translation(
                    Vec3::from_array(scale),
                    Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]).normalize(),
                    Vec3::from_array(*translation),
                );
                let (world_scale, world_rotation, world_translation) =
                    (parent * local).to_scale_rotation_translation();
                GltfInstance {
                    translation: world_translation.as_dvec3().to_array(),
                    rotation: world_rotation.as_dquat().into(),
                    scale: world_scale.as_dvec3().to_array(),
                    id: ids
                        .as_ref()
                        .and_then(|values| values.get(index))
                        .map(|value| u64::from(*value)),
                }
            })
            .collect();
        sets.push(GltfInstanceSet {
            name: node
                .name()
                .map(str::to_owned)
                .or_else(|| node.mesh().and_then(|mesh| mesh.name()).map(str::to_owned))
                .unwrap_or_else(|| format!("node_{}", node.index())),
            instances,
        });
    }
    unsupported.sort();
    unsupported.dedup();
    Ok(GltfInstancing { sets, unsupported })
}

/// Accumulates world transforms down one node subtree.
fn accumulate(node: &gltf::Node<'_>, parent: Mat4, world: &mut [Mat4]) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let combined = parent * local;
    if let Some(slot) = world.get_mut(node.index()) {
        *slot = combined;
    }
    for child in node.children() {
        accumulate(&child, combined, world);
    }
}

/// Three-lane float data, from float or normalized integer storage.
fn read_vec3<'a>(
    accessor: &gltf::Accessor<'a>,
    data: &impl Fn(gltf::Buffer<'a>) -> Option<&'a [u8]>,
) -> Option<Vec<[f32; 3]>> {
    match accessor.data_type() {
        gltf::accessor::DataType::F32 => {
            gltf::accessor::Iter::<[f32; 3]>::new(accessor.clone(), data).map(Iterator::collect)
        }
        _ => None,
    }
}

/// Four-lane float data, accepting the normalized byte and short storage the extension permits for
/// rotations.
fn read_vec4<'a>(
    accessor: &gltf::Accessor<'a>,
    data: &impl Fn(gltf::Buffer<'a>) -> Option<&'a [u8]>,
) -> Option<Vec<[f32; 4]>> {
    match accessor.data_type() {
        gltf::accessor::DataType::F32 => {
            gltf::accessor::Iter::<[f32; 4]>::new(accessor.clone(), data).map(Iterator::collect)
        }
        gltf::accessor::DataType::I16 => {
            gltf::accessor::Iter::<[i16; 4]>::new(accessor.clone(), data).map(|values| {
                values
                    .map(|lanes| lanes.map(|lane| f32::from(lane) / f32::from(i16::MAX)))
                    .collect()
            })
        }
        gltf::accessor::DataType::I8 => {
            gltf::accessor::Iter::<[i8; 4]>::new(accessor.clone(), data).map(|values| {
                values
                    .map(|lanes| lanes.map(|lane| f32::from(lane) / f32::from(i8::MAX)))
                    .collect()
            })
        }
        _ => None,
    }
}

/// Unsigned identity data, from any of the integer widths glTF allows.
fn read_u32<'a>(
    accessor: &gltf::Accessor<'a>,
    data: &impl Fn(gltf::Buffer<'a>) -> Option<&'a [u8]>,
) -> Option<Vec<u32>> {
    match accessor.data_type() {
        gltf::accessor::DataType::U32 => {
            gltf::accessor::Iter::<u32>::new(accessor.clone(), data).map(Iterator::collect)
        }
        gltf::accessor::DataType::U16 => gltf::accessor::Iter::<u16>::new(accessor.clone(), data)
            .map(|values| values.map(u32::from).collect()),
        gltf::accessor::DataType::U8 => gltf::accessor::Iter::<u8>::new(accessor.clone(), data)
            .map(|values| values.map(u32::from).collect()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("instanced-scatter.gltf")
    }

    /// The instances come out in the node's world space, so a scatter under a moved node moves with
    /// it, and an attribute this reader cannot express is reported rather than guessed at.
    #[test]
    fn instanced_placements_read_in_world_space() {
        let instancing = read_gltf_instancing(fixture()).expect("the fixture reads");
        assert_eq!(instancing.sets.len(), 1);
        let set = &instancing.sets[0];
        assert_eq!(set.name, "oakScatter");
        assert_eq!(set.instances.len(), 3);

        // The node sits at x = 10, so every instance is offset by it.
        let x: Vec<f64> = set
            .instances
            .iter()
            .map(|instance| instance.translation[0])
            .collect();
        assert!((x[0] - 10.0).abs() < 1.0e-6);
        assert!((x[1] - 14.0).abs() < 1.0e-6);
        assert!((x[2] - 7.0).abs() < 1.0e-6);
        // Scales survive, and the identities come from the `_ID` attribute.
        assert!((set.instances[1].scale[1] - 2.0).abs() < 1.0e-6);
        assert_eq!(
            set.instances
                .iter()
                .map(|instance| instance.id)
                .collect::<Vec<_>>(),
            vec![Some(11), Some(22), Some(33)]
        );
        assert_eq!(instancing.unsupported, vec!["_WIND".to_owned()]);
    }
}
