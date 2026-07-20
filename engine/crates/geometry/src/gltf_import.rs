//! glTF (`.gltf`/`.glb`) import onto the `gltf` crate.
//!
//! The `gltf` crate exposes an index-only typed API. Two deterministic glue pieces are
//! reconstructed by hand and pinned by tests so the output stays byte-stable for the
//! asset bake hashes that depend on it:
//!
//! - **The parent map** ([`build_parents`]) — the crate's `Node` exposes `children`,
//!   not a parent, so parent indices are built by one walk over every node's children.
//! - **Node ordering** — joint-index and channel→node resolution depend on the node
//!   array being in document order. The crate iterates `nodes()` in document order and
//!   exposes `node.index()`; both index the same array.
//!
//! Every import — skinned, unskinned-multinode, OBJ, or static — produces a node forest
//! ([`build_node_forest`]) whose mesh-bearing nodes carry a node-local [`Mesh`], plus one
//! heterogeneous [`AnimClip`] set ([`decode_clips`]) of bone, node, and morph-weight
//! tracks. Geometry stays node-local (no world-transform bake), so a node a node-TRS
//! track drives keeps its drivable local transform.

use std::path::Path;

use glam::{Affine3A, Mat4, Quat, Vec2, Vec3, Vec4};
use gltf::animation::Interpolation;
use gltf::mesh::Mode;

use crate::error::{Error, Result};
use crate::picking::generate_normals;
use crate::types::{
    AlphaMode, AnimClip, AnimInterp, AnimPath, AnimTarget, AnimTrack, ImportedMaterial,
    ImportedModel, ImportedNode, ImportedSkin, Mesh, MorphData, MorphDelta, MorphTarget,
    SkinPayload, Submesh, TextureSource, Vertex, VertexSkin,
};

/// Below this squared magnitude a morph delta is treated as zero and dropped, keeping the
/// sparse delta bank to genuinely moved vertices.
const MORPH_DELTA_EPSILON_SQ: f32 = 1e-12;

/// Import a `.gltf`/`.glb` model into the in-memory [`ImportedModel`] graph.
///
/// Decodes a node forest with node-local meshes (first-seen material slots), one
/// heterogeneous clip set (bone, node, and morph-weight tracks), the optional skin
/// payload (joint list, inverse-bind matrices, the skinned mesh node's per-vertex
/// stream), and the materials with their texture byte blobs. The skin is imported only
/// when the first skin covers every triangle primitive; a mixed skinned/unskinned model
/// imports as plain geometry.
pub fn import_gltf_model(path: impl AsRef<Path>) -> Result<ImportedModel> {
    let path = path.as_ref();
    let gltf = gltf::Gltf::open(path)
        .map_err(|e| Error::Import(format!("gltf: cannot parse '{}': {e}", path.display())))?;
    let document = gltf.document;
    let base = path.parent();
    let buffers = gltf::import_buffers(&document, base, gltf.blob).map_err(|e| {
        Error::Import(format!(
            "gltf: cannot load buffers for '{}': {e}",
            path.display()
        ))
    })?;

    let parents = build_parents(&document);
    let mut nodes = build_node_forest(&document, &parents);

    let mut saw_skinned = false;
    let mut saw_unskinned = false;
    // Distinct source materials in first-seen order, keyed by the material's document
    // index (`None` == a primitive with no material, which gets the default slot).
    let mut material_table: Vec<Option<usize>> = Vec::new();
    let mut material_slot_of = |key: Option<usize>| -> u32 {
        if let Some(pos) = material_table.iter().position(|m| *m == key) {
            return pos as u32;
        }
        let slot = material_table.len() as u32;
        material_table.push(key);
        slot
    };

    // Append every mesh-bearing node's primitives into that node's local mesh — no world
    // bake. For a skinned node, keep its per-vertex skin stream; the skin's mesh node
    // supplies the payload stream. Morph deltas are sparse-compacted per mesh; the model
    // carries one mesh-global `MorphData` (the first mesh-bearing node that has targets).
    let mut skin_streams: Vec<(usize, Vec<VertexSkin>)> = Vec::new();
    let mut model_morph: Option<MorphData> = None;
    for node in document.nodes() {
        let Some(node_mesh) = node.mesh() else {
            continue;
        };
        let mut local = Mesh::default();
        let mut local_skins: Vec<VertexSkin> = Vec::new();
        let mut local_morph = MorphData::default();
        for prim in node_mesh.primitives() {
            append_primitive(
                &prim,
                &buffers,
                &mut local,
                &mut local_skins,
                &mut local_morph,
                &mut saw_skinned,
                &mut saw_unskinned,
                &mut material_slot_of,
                path,
            )?;
        }
        if local.vertices.is_empty() {
            continue;
        }
        if !any_normals_present(&local) {
            generate_normals(&mut local);
        }
        // Recompute tangents when the source omitted the TANGENT accessor (any vertex left with a
        // zero tangent), so every mesh has a UV-aligned frame for normal maps + vector displacement.
        if local
            .vertices
            .iter()
            .any(|v| v.tangent[0] == 0.0 && v.tangent[1] == 0.0 && v.tangent[2] == 0.0)
        {
            crate::compute_tangents(&mut local);
        }
        if node.skin().is_some() {
            skin_streams.push((node.index(), std::mem::take(&mut local_skins)));
        }
        if !local_morph.targets.is_empty() {
            finalize_morph(&mut local_morph, &node_mesh, path);
            if model_morph.is_none() {
                model_morph = Some(local_morph);
            } else {
                tracing::warn!(
                    "gltf: '{}' has morph targets on more than one mesh node; only the first is kept",
                    path.display()
                );
            }
        }
        nodes[node.index()].mesh = Some(local);
    }

    let mut materials: Vec<ImportedMaterial> = Vec::with_capacity(material_table.len());
    for key in &material_table {
        match key {
            Some(index) => {
                let src = document
                    .materials()
                    .find(|m| m.index() == Some(*index))
                    .ok_or_else(|| Error::Import(format!("gltf: material {index} vanished")))?;
                materials.push(extract_gltf_material(&src, &buffers, path));
            }
            None => materials.push(ImportedMaterial::default()),
        }
    }
    if materials.is_empty() {
        materials.push(ImportedMaterial::default());
    }

    // Skin payload: only when the FIRST skin covers every triangle primitive (a mixed
    // skinned/unskinned model would deform unweighted vertices to the origin, so it
    // imports as plain geometry instead). The payload stream is the skinned mesh node's
    // per-vertex influences.
    let skins_count = document.skins().count();
    let mut skin: Option<SkinPayload> = None;
    if skins_count > 0 && saw_skinned && !saw_unskinned {
        let desc = build_skin_desc(&document, &buffers);
        let stream = skin_streams
            .iter()
            .find(|(idx, _)| *idx as i32 == desc.mesh_node)
            .or_else(|| skin_streams.first())
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        skin = Some(SkinPayload { stream, desc });
    } else if saw_skinned && saw_unskinned {
        tracing::warn!(
            "gltf: '{}' mixes skinned and unskinned primitives; importing unskinned",
            path.display()
        );
    }

    let animations = {
        let joints: &[i32] = skin
            .as_ref()
            .map(|s| s.desc.joints.as_slice())
            .unwrap_or(&[]);
        decode_clips(&document, &buffers, joints, &nodes, path)
    };

    if nodes.iter().all(|n| n.mesh.is_none()) {
        return Err(Error::Import(format!(
            "gltf: '{}' has no triangle geometry",
            path.display()
        )));
    }

    Ok(ImportedModel {
        nodes,
        materials,
        animations,
        skin,
        morph: model_morph,
    })
}

/// Fill in a decoded morph target's rest weights and names from mesh-level metadata.
/// Cross-primitive target-count disagreement was already reconciled per-primitive in
/// [`append_primitive`]. The canonical count is the mesh `weights` length when it disagrees
/// with the decoded target count, padding or trimming with a warning. Names come from the
/// exporter convention `extras.targetNames`; missing entries use `morph_{k}`.
fn finalize_morph(morph: &mut MorphData, node_mesh: &gltf::Mesh, path: &Path) {
    let weights = node_mesh.weights().unwrap_or(&[]);
    if !weights.is_empty() && weights.len() != morph.targets.len() {
        tracing::warn!(
            "gltf: '{}' mesh weights ({}) disagree with morph target count ({}); reconciling to the weights length",
            path.display(),
            weights.len(),
            morph.targets.len()
        );
        morph
            .targets
            .resize_with(weights.len(), MorphTarget::default);
    }
    let names = morph_target_names(node_mesh, morph.targets.len(), path);
    for (k, target) in morph.targets.iter_mut().enumerate() {
        target.rest_weight = weights.get(k).copied().unwrap_or(0.0);
        target.name = names
            .get(k)
            .and_then(Clone::clone)
            .unwrap_or_else(|| format!("morph_{k}"));
    }
}

/// Read the glTF exporter convention `mesh.extras.targetNames`, preserving target order.
fn morph_target_names(
    node_mesh: &gltf::Mesh,
    target_count: usize,
    path: &Path,
) -> Vec<Option<String>> {
    let Some(raw) = node_mesh.extras().as_ref() else {
        return vec![None; target_count];
    };
    let Ok(extras) = serde_json::from_str::<serde_json::Value>(raw.get()) else {
        tracing::warn!(
            "gltf: '{}' mesh extras are not valid JSON; morph target names were synthesized",
            path.display()
        );
        return vec![None; target_count];
    };
    let Some(values) = extras
        .get("targetNames")
        .and_then(serde_json::Value::as_array)
    else {
        return vec![None; target_count];
    };
    if values.len() != target_count {
        tracing::warn!(
            "gltf: '{}' morph target name count ({}) disagrees with the target count ({target_count}); missing names were synthesized",
            path.display(),
            values.len()
        );
    }
    (0..target_count)
        .map(|index| {
            values
                .get(index)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

/// Build the parent index of every node (`-1` for a root).
///
/// The `gltf` crate's `Node` exposes its children, not its parent, so one pass over
/// every node's children fills the inverse map.
fn build_parents(document: &gltf::Document) -> Vec<i32> {
    let mut parents = vec![-1i32; document.nodes().count()];
    for node in document.nodes() {
        let parent = node.index() as i32;
        for child in node.children() {
            parents[child.index()] = parent;
        }
    }
    parents
}

/// Build the imported node forest in document order: name, parent index, and local TRS
/// for every node. Mesh payloads are filled in by [`import_gltf_model`]; every node here
/// starts with `mesh: None`.
fn build_node_forest(document: &gltf::Document, parents: &[i32]) -> Vec<ImportedNode> {
    let mut nodes: Vec<ImportedNode> = Vec::with_capacity(document.nodes().count());
    for node in document.nodes() {
        let index = node.index();
        let name = node
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Node {index}"));
        let (translation, rotation, scale) = match node.transform() {
            gltf::scene::Transform::Matrix { matrix } => {
                let (s, r, t) = Affine3A::from_mat4(Mat4::from_cols_array_2d(&matrix))
                    .to_scale_rotation_translation();
                (t, r, s)
            }
            gltf::scene::Transform::Decomposed {
                translation,
                rotation,
                scale,
            } => (
                Vec3::from_array(translation),
                // glam's Quat is xyzw, matching the glTF storage order.
                Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]),
                Vec3::from_array(scale),
            ),
        };
        nodes.push(ImportedNode {
            name,
            parent: parents[index],
            translation,
            rotation,
            scale,
            mesh: None,
        });
    }
    nodes
}

/// Read one triangle primitive's attributes and append the resulting vertex/index/
/// submesh data into the node-local mesh (object space, no transform bake).
///
/// Non-triangle primitives and primitives without positions are skipped. Tracks whether
/// a skinned (joints + weights) or an unskinned primitive was seen, and the first-seen
/// material slot.
#[allow(clippy::too_many_arguments)]
fn append_primitive(
    prim: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
    mesh: &mut Mesh,
    vertex_skins: &mut Vec<VertexSkin>,
    morph: &mut MorphData,
    saw_skinned: &mut bool,
    saw_unskinned: &mut bool,
    material_slot_of: &mut impl FnMut(Option<usize>) -> u32,
    path: &Path,
) -> Result<()> {
    if prim.mode() != Mode::Triangles {
        return Ok(());
    }
    let reader = prim.reader(|buffer| buffers.get(buffer.index()).map(|d| d.0.as_slice()));

    let Some(positions) = reader.read_positions() else {
        return Ok(());
    };
    let positions: Vec<[f32; 3]> = positions.collect();
    let vertex_count = positions.len();

    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|it| it.collect());
    let texcoords: Option<Vec<[f32; 2]>> =
        reader.read_tex_coords(0).map(|it| it.into_f32().collect());
    // The glTF TANGENT accessor is already UV-aligned with the ±1 handedness in `w`; when absent
    // (or partially present across primitives) the node-mesh finalize recomputes with `compute_tangents`.
    let tangents: Option<Vec<[f32; 4]>> = reader.read_tangents().map(|it| it.collect());
    let joints: Option<Vec<[u16; 4]>> = reader.read_joints(0).map(|it| it.into_u16().collect());
    let weights: Option<Vec<[f32; 4]>> = reader.read_weights(0).map(|it| it.into_f32().collect());

    let is_skinned = joints.is_some() && weights.is_some();
    if is_skinned {
        *saw_skinned = true;
    } else {
        *saw_unskinned = true;
    }

    let material_slot = material_slot_of(prim.material().index());

    let vertex_offset = mesh.vertices.len() as i32;
    let first_index = mesh.indices.len() as u32;

    for i in 0..vertex_count {
        let position = Vec3::from_array(positions[i]);
        let normal = match &normals {
            Some(ns) => Vec3::from_array(ns[i]),
            None => Vec3::ZERO,
        };
        let uv0 = match &texcoords {
            Some(uvs) => Vec2::from_array(uvs[i]),
            None => Vec2::ZERO,
        };
        let tangent = match &tangents {
            Some(ts) => ts[i],
            None => [0.0, 0.0, 0.0, 0.0],
        };
        mesh.vertices.push(Vertex {
            position,
            normal,
            uv0,
            tangent,
        });

        let mut influence = VertexSkin::default();
        if let (Some(js), Some(ws)) = (&joints, &weights) {
            influence.joints = js[i];
            influence.weights = ws[i];
        }
        vertex_skins.push(influence);
    }

    // Sparse morph deltas: the `gltf` reader resolves sparse accessors internally and
    // hands back a dense iterator per target; we compact to genuinely moved vertices and
    // shift each delta's index by this primitive's base in the node-local mesh.
    for (ti, (positions, normals, _tangents)) in reader.read_morph_targets().enumerate() {
        let dps: Vec<[f32; 3]> = positions.map(|it| it.collect()).unwrap_or_default();
        let dns: Vec<[f32; 3]> = normals.map(|it| it.collect()).unwrap_or_default();
        if morph.targets.len() <= ti {
            morph.targets.resize_with(ti + 1, MorphTarget::default);
        }
        let target = &mut morph.targets[ti];
        for v in 0..vertex_count {
            let dp = dps
                .get(v)
                .copied()
                .map(Vec3::from_array)
                .unwrap_or(Vec3::ZERO);
            let dn = dns
                .get(v)
                .copied()
                .map(Vec3::from_array)
                .unwrap_or(Vec3::ZERO);
            if dp.length_squared() > MORPH_DELTA_EPSILON_SQ
                || dn.length_squared() > MORPH_DELTA_EPSILON_SQ
            {
                target.deltas.push(MorphDelta {
                    vertex_index: vertex_offset as u32 + v as u32,
                    d_position: dp,
                    d_normal: dn,
                });
            }
        }
    }

    match reader.read_indices() {
        Some(indices) => {
            for index in indices.into_u32() {
                if index as usize >= vertex_count {
                    return Err(Error::Import(format!(
                        "gltf: '{}' has an out-of-range index",
                        path.display()
                    )));
                }
                mesh.indices.push(index);
            }
        }
        None => {
            for i in 0..vertex_count as u32 {
                mesh.indices.push(i);
            }
        }
    }

    let index_count = mesh.indices.len() as u32 - first_index;
    mesh.submeshes.push(Submesh {
        first_index,
        index_count,
        vertex_offset,
        material_slot,
    });
    Ok(())
}

/// Build the [`ImportedSkin`] descriptor from the document's first skin: the joint index
/// list, the inverse-bind matrices, the skeleton root, and the skinned mesh node.
fn build_skin_desc(document: &gltf::Document, buffers: &[gltf::buffer::Data]) -> ImportedSkin {
    let gltf_skin = document.skins().next().expect("skins_count > 0");

    let joints: Vec<i32> = gltf_skin.joints().map(|j| j.index() as i32).collect();
    let joint_count = joints.len();

    let mut inverse_bind = vec![Mat4::IDENTITY; joint_count];
    let skin_reader =
        gltf_skin.reader(|buffer| buffers.get(buffer.index()).map(|d| d.0.as_slice()));
    if let Some(ibms) = skin_reader.read_inverse_bind_matrices() {
        for (j, m) in ibms.enumerate().take(joint_count) {
            inverse_bind[j] = Mat4::from_cols_array_2d(&m);
        }
    }

    let skeleton_root = gltf_skin.skeleton().map(|n| n.index() as i32).unwrap_or(-1);

    let mut mesh_node = -1i32;
    for node in document.nodes() {
        let is_this_skin = node.skin().map(|s| s.index()) == Some(gltf_skin.index());
        if is_this_skin && node.mesh().is_some() {
            mesh_node = node.index() as i32;
            break;
        }
    }

    ImportedSkin {
        joints,
        inverse_bind,
        skeleton_root,
        mesh_node,
    }
}

/// Decode the document's clips into heterogeneous tracks.
///
/// A channel targeting a node in `joints` decodes as a [`AnimTarget::Bone`] track keyed
/// by joint position; any other node decodes as a [`AnimTarget::Node`] track bound by
/// name. A morph-weights channel decodes as an [`AnimPath::Weights`] node track carrying
/// the N-wide weight stream. A genuinely empty sampler is skipped with a warning; sparse
/// samplers resolve internally through the `gltf` reader. A clip with no surviving tracks
/// is dropped.
fn decode_clips(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    joints: &[i32],
    nodes: &[ImportedNode],
    path: &Path,
) -> Vec<AnimClip> {
    use gltf::animation::util::ReadOutputs;
    let mut clips: Vec<AnimClip> = Vec::new();
    for (a, anim) in document.animations().enumerate() {
        let name = anim
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("clip_{a}"));
        let mut clip = AnimClip {
            name,
            duration: 0.0,
            tracks: Vec::new(),
        };
        for channel in anim.channels() {
            let target = channel.target();
            let node_index = target.node().index();
            let target_name = nodes
                .get(node_index)
                .map(|n| n.name.clone())
                .unwrap_or_default();

            let sampler = channel.sampler();
            let interp = to_track_interp(sampler.interpolation());
            let stride = if interp == AnimInterp::CubicSpline {
                3
            } else {
                1
            };

            let reader =
                channel.reader(|buffer| buffers.get(buffer.index()).map(|d| d.0.as_slice()));
            let (Some(inputs), Some(outputs)) = (reader.read_inputs(), reader.read_outputs())
            else {
                tracing::warn!(
                    "gltf: '{}' clip '{}' has an empty sampler; channel skipped",
                    path.display(),
                    clip.name
                );
                continue;
            };
            let times: Vec<f32> = inputs.collect();

            // A node-TRS channel binds to a bone when its node is a skin joint, else to a
            // plain node by name. A morph-weights channel is always a node target.
            let (bone_target, bone_index) =
                match joints.iter().position(|j| *j == node_index as i32) {
                    Some(pos) => (AnimTarget::Bone, pos as i32),
                    None => (AnimTarget::Node, -1),
                };

            let (path_kind, anim_target, index, morph_count, values) = match outputs {
                ReadOutputs::Translations(it) => {
                    let mut v = Vec::with_capacity(times.len() * 3 * stride);
                    for t in it {
                        v.extend_from_slice(&t);
                    }
                    (AnimPath::Translation, bone_target, bone_index, 0, v)
                }
                ReadOutputs::Scales(it) => {
                    let mut v = Vec::with_capacity(times.len() * 3 * stride);
                    for s in it {
                        v.extend_from_slice(&s);
                    }
                    (AnimPath::Scale, bone_target, bone_index, 0, v)
                }
                ReadOutputs::Rotations(it) => {
                    let mut v = Vec::with_capacity(times.len() * 4 * stride);
                    for r in it.into_f32() {
                        v.extend_from_slice(&r);
                    }
                    (AnimPath::Rotation, bone_target, bone_index, 0, v)
                }
                ReadOutputs::MorphTargetWeights(w) => {
                    let v: Vec<f32> = w.into_f32().collect();
                    let denom = times.len().max(1) * stride;
                    let morph_count = (v.len() / denom) as u32;
                    (AnimPath::Weights, AnimTarget::Node, -1, morph_count, v)
                }
            };

            let track = AnimTrack {
                target: anim_target,
                index,
                target_name,
                path: path_kind,
                interp,
                morph_count,
                times,
                values,
            };
            if let Some(&last) = track.times.last() {
                if last > clip.duration {
                    clip.duration = last;
                }
            }
            clip.tracks.push(track);
        }
        if !clip.tracks.is_empty() {
            clips.push(clip);
        }
    }
    clips
}

/// Map a glTF sampler interpolation onto the engine's [`AnimInterp`].
fn to_track_interp(interp: Interpolation) -> AnimInterp {
    match interp {
        Interpolation::Step => AnimInterp::Step,
        Interpolation::CubicSpline => AnimInterp::CubicSpline,
        Interpolation::Linear => AnimInterp::Linear,
    }
}

/// Extract a material's PBR factors and texture byte blobs.
fn extract_gltf_material(
    src: &gltf::Material,
    buffers: &[gltf::buffer::Data],
    path: &Path,
) -> ImportedMaterial {
    let alpha_mode = match src.alpha_mode() {
        gltf::material::AlphaMode::Mask => AlphaMode::Mask,
        gltf::material::AlphaMode::Blend => AlphaMode::Blend,
        gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
    };
    let mut material = ImportedMaterial {
        name: src.name().map(str::to_owned).unwrap_or_default(),
        emissive: Vec3::from_array(src.emissive_factor()),
        emissive_strength: src.emissive_strength().unwrap_or(1.0),
        alpha_mode,
        alpha_cutoff: src.alpha_cutoff().unwrap_or(0.5),
        double_sided: src.double_sided(),
        normal: src
            .normal_texture()
            .and_then(|t| read_texture_bytes(&t.texture(), buffers, "normal", path)),
        occlusion: src
            .occlusion_texture()
            .and_then(|t| read_texture_bytes(&t.texture(), buffers, "occlusion", path)),
        emissive_tex: src
            .emissive_texture()
            .and_then(|t| read_texture_bytes(&t.texture(), buffers, "emissive", path)),
        ..Default::default()
    };

    let pbr = src.pbr_metallic_roughness();
    material.base_color = Vec4::from_array(pbr.base_color_factor());
    material.metallic = pbr.metallic_factor();
    material.roughness = pbr.roughness_factor();
    material.albedo = pbr
        .base_color_texture()
        .and_then(|t| read_texture_bytes(&t.texture(), buffers, "albedo", path));
    material.metallic_roughness = pbr
        .metallic_roughness_texture()
        .and_then(|t| read_texture_bytes(&t.texture(), buffers, "metallic-roughness", path));
    material
}

/// Read a texture's encoded image bytes from an embedded buffer view or external
/// file, percent-decoding the URI. A `data:` URI is logged and skipped.
fn read_texture_bytes(
    texture: &gltf::Texture,
    buffers: &[gltf::buffer::Data],
    label: &str,
    path: &Path,
) -> Option<TextureSource> {
    let source = texture.source().source();
    match source {
        gltf::image::Source::View { view, mime_type } => {
            let buffer = &buffers[view.buffer().index()];
            let start = view.offset();
            let end = start + view.length();
            let bytes = buffer.0.get(start..end)?.to_vec();
            if bytes.is_empty() {
                return None;
            }
            Some(TextureSource {
                bytes,
                ext: extension_from_mime(mime_type),
            })
        }
        gltf::image::Source::Uri { uri, .. } => {
            if uri.starts_with("data:") {
                tracing::warn!(
                    "gltf: '{}' embeds its {label} as a data: URI (not yet supported)",
                    path.display()
                );
                return None;
            }
            let decoded = percent_decode(uri);
            let dir = path.parent().unwrap_or_else(|| Path::new("."));
            let full = dir.join(&decoded);
            let bytes = std::fs::read(&full).ok()?;
            Some(TextureSource {
                bytes,
                ext: extension_of(&decoded),
            })
        }
    }
}

/// Map an image MIME type onto a file extension.
fn extension_from_mime(mime: &str) -> String {
    match mime {
        "image/jpeg" => "jpg".to_owned(),
        _ => "png".to_owned(),
    }
}

/// The extension of a path (the substring after the last `.`), empty if none.
fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) => name[dot + 1..].to_owned(),
        None => String::new(),
    }
}

/// Percent-decode a URI in place (`%20` -> space).
fn percent_decode(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether any vertex carries a non-zero normal (the importer recomputes normals only
/// when a source provides none).
fn any_normals_present(mesh: &Mesh) -> bool {
    mesh.vertices.iter().any(|v| v.normal.dot(v.normal) > 1e-12)
}
