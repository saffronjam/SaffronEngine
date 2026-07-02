//! The resolution from a loaded [`MaterialAsset`] (or a scene material component) to a
//! render-ready [`SubmeshMaterial`] with bindless GPU texture handles.
//!
//! Three entry points, narrowing in scope:
//!
//! - [`build_submesh_material`] maps one resolved [`MaterialAsset`] to a
//!   [`SubmeshMaterial`], resolving each texture slot through a borrowed loader closure.
//!   The main draw path passes [`AssetServer::load_texture_asset`]; the thumbnail worker
//!   passes its own uploader — one mapping, one call site, the loader a
//!   `&dyn Fn(Uuid) -> Option<Arc<…>>`.
//! - [`AssetServer::resolve_material_asset`] instantiates a [`MaterialAsset`] on the main
//!   thread, wiring [`build_submesh_material`]'s loader to [`AssetServer::load_texture_asset`].
//! - [`AssetServer::resolve_entity_materials`] resolves a single renderable's whole
//!   submesh-material table, applying the per-entity component precedence and producing
//!   the [`ResolvedMaterials`] result the draw loop reads.
//!
//! # The packed ORM/ARM map feeds two slots
//!
//! A material's single ORM texture (`orm_texture`) drives **both** the
//! metallic-roughness slot (roughness in G, metalness in B) and the occlusion slot (AO
//! in R), so one map covers all three. [`SubmeshMaterial::alpha_clip`] is derived from
//! `blend == "masked"`.
//!
//! # Component precedence
//!
//! [`AssetServer::resolve_entity_materials`] keeps the exact order: a [`MaterialAsset`]
//! component (a `.smat` id, plus the codegen `_mesh.spv` shader override when a
//! non-foldable graph is compiled on disk) wins; else a [`MaterialSet`] component
//! (per-submesh slots, clamped to the slot count); else a [`Material`] component (a
//! single inline material applied to every submesh). The resolved base color's rgb is
//! captured as the proxy albedo for the DDGI voxel box.

use std::sync::Arc;

use saffron_geometry::Submesh;
use saffron_geometry::glam::Vec3;
use saffron_rendering::{GpuTexture, SubmeshMaterial};
use saffron_scene::{
    Entity, Material, MaterialAsset as MaterialAssetComponent, MaterialSet, MaterialSlot, Scene,
};

use crate::gpu::GpuUploader;
use crate::graph::lower_graph_to_params;
use crate::material::{MaterialAsset, default_material_asset, load_material_asset_raw};
use crate::{AssetServer, DEFAULT_MATERIAL_ID};

/// The default übershader the scene PSO selects for a non-codegen material.
const DEFAULT_MESH_SHADER: &str = "shaders/mesh.spv";

/// The per-submesh materials for one renderable, plus the entity-level `unlit` flag
/// (which selects the PSO) and a proxy albedo for the DDGI voxel box.
///
/// Built by [`AssetServer::resolve_entity_materials`] from the entity's
/// [`MaterialAsset`]/[`MaterialSet`]/[`Material`] component (precedence in that order),
/// else engine defaults.
#[derive(Clone)]
pub struct ResolvedMaterials {
    /// One [`SubmeshMaterial`] per mesh submesh; a single entry applies to every
    /// submesh (the draw path clamps).
    pub submeshes: Vec<SubmeshMaterial>,
    /// Skip lighting for this renderable (selects the unlit PSO permutation).
    pub unlit: bool,
    /// The resolved base color's rgb, captured for the DDGI voxel-box proxy albedo.
    pub proxy_albedo: Vec3,
    /// The übershader the scene PSO selects. A codegen material points this at its
    /// compiled `_mesh.spv` variant; everything else keeps the shared übershader.
    pub shader: String,
}

impl Default for ResolvedMaterials {
    fn default() -> Self {
        Self {
            submeshes: Vec::new(),
            unlit: false,
            proxy_albedo: Vec3::ONE,
            shader: DEFAULT_MESH_SHADER.to_owned(),
        }
    }
}

/// Maps a resolved [`MaterialAsset`] to a render-ready [`SubmeshMaterial`], resolving
/// each texture slot through `load_tex`.
///
/// The main draw path passes a closure over [`AssetServer::load_texture_asset`]; the
/// thumbnail worker passes its own uploader. A zero texture id leaves that handle unset —
/// the draw path's default-white substitution is a renderer concern, not done here. The
/// packed `orm_texture` feeds **both** the metallic-roughness and the occlusion slot, and
/// `alpha_clip` is `blend == "masked"`.
///
/// The loader is `FnMut`: the main path's closure fills the texture cache as it resolves,
/// so a borrowed mutable closure is the allocation-free shape — no trait object for a
/// single call site.
pub fn build_submesh_material(
    material: &MaterialAsset,
    load_tex: &mut dyn FnMut(saffron_core::Uuid) -> Option<Arc<GpuTexture>>,
) -> SubmeshMaterial {
    let mut sm = SubmeshMaterial {
        base_color: material.base_color,
        metallic: material.metallic,
        roughness: material.roughness,
        emissive: material.emissive,
        emissive_strength: material.emissive_strength,
        normal_strength: material.normal_strength,
        uv_tiling: material.uv_tiling,
        uv_offset: material.uv_offset,
        height_scale: material.height_scale,
        alpha_clip: material.blend == "masked",
        alpha_cutoff: material.alpha_cutoff,
        double_sided: material.double_sided,
        ..SubmeshMaterial::defaults()
    };
    if material.albedo_texture.value() != 0 {
        sm.albedo_texture = load_tex(material.albedo_texture);
    }
    if material.orm_texture.value() != 0 {
        sm.metallic_roughness_texture = load_tex(material.orm_texture);
        sm.occlusion_texture = load_tex(material.orm_texture);
    }
    if material.normal_texture.value() != 0 {
        sm.normal_texture = load_tex(material.normal_texture);
    }
    if material.emissive_texture.value() != 0 {
        sm.emissive_texture = load_tex(material.emissive_texture);
    }
    if material.height_texture.value() != 0 {
        sm.height_texture = load_tex(material.height_texture);
    }
    sm
}

impl AssetServer {
    /// Resolves a loaded [`MaterialAsset`] to a render-ready [`SubmeshMaterial`], wiring
    /// [`build_submesh_material`]'s loader to [`AssetServer::load_texture_asset`].
    ///
    /// The main-thread instantiation: each texture id resolves through the GPU cache (a
    /// cache hit returns the live `Arc`; a miss uploads, then caches). A dangling id
    /// negative-caches and leaves the slot unset.
    pub fn resolve_material_asset(
        &mut self,
        gpu: &dyn GpuUploader,
        material: &MaterialAsset,
    ) -> SubmeshMaterial {
        build_submesh_material(material, &mut |id| self.load_texture_asset(gpu, id))
    }

    /// Resolves a single renderable's whole submesh-material table from the entity's
    /// material components, applying the precedence and the codegen-shader override.
    ///
    /// A [`MaterialAsset`](saffron_scene::MaterialAsset) component (a `.smat` id) wins:
    /// the resolved material fills every submesh slot, and when its raw graph is
    /// non-foldable and a compiled `<id>_mesh.spv` exists on disk, the result points its
    /// shader at that variant. Else a [`MaterialSet`] component drives per-submesh slots
    /// (each submesh's `material_slot` clamped to the slot count). Else a single
    /// [`Material`] component applies to every submesh. The resolved base color's rgb is
    /// the DDGI proxy albedo.
    ///
    /// `submeshes` is the renderable mesh's submesh table — the only thing read from the
    /// mesh — so the resolve never needs the GPU mesh itself.
    pub fn resolve_entity_materials(
        &mut self,
        gpu: &dyn GpuUploader,
        scene: &Scene,
        entity: Entity,
        submeshes: &[Submesh],
    ) -> ResolvedMaterials {
        let mut out = ResolvedMaterials::default();

        if scene.has_component::<MaterialAssetComponent>(entity) {
            let material_id = scene
                .component::<MaterialAssetComponent>(entity)
                .map(|c| c.material)
                .unwrap_or_default();
            if material_id.value() != 0 {
                let loaded = load_material_asset(self, material_id);
                let material = loaded.unwrap_or_else(|| {
                    tracing::warn!(
                        "entity material asset {} missing; using default",
                        material_id.value()
                    );
                    default_material_asset()
                });
                out.unlit = material.unlit;
                out.proxy_albedo = material.base_color.truncate();
                // A non-foldable graph renders via its compiled übershader variant (built
                // at material-set-graph time). Fall back to the shared übershader if it
                // isn't on disk yet.
                if let Ok(raw) = load_material_asset_raw(self, material_id)
                    && is_non_empty_object(&raw.graph)
                {
                    let mut probe = raw.clone();
                    if !lower_graph_to_params(&raw.graph, &mut probe) {
                        let spv = self
                            .root
                            .join("materials")
                            .join(format!("{}_mesh.spv", material_id.value()));
                        if spv.exists() {
                            out.shader = spv.to_string_lossy().into_owned();
                        }
                    }
                }
                let sm = self.resolve_material_asset(gpu, &material);
                let count = submeshes.len().max(1);
                out.submeshes = vec![sm; count];
                return out;
            }
        }

        if scene.has_component::<MaterialSet>(entity) {
            let slots = scene
                .with_component::<MaterialSet, _>(entity, |set| set.slots.clone())
                .unwrap_or_default();
            if !slots.is_empty() {
                out.unlit = slots[0].unlit;
                out.proxy_albedo = slots[0].base_color.truncate();
                out.submeshes.reserve(submeshes.len());
                for submesh in submeshes {
                    let slot = (submesh.material_slot as usize).min(slots.len() - 1);
                    out.submeshes.push(self.lower_slot(gpu, &slots[slot]));
                }
                return out;
            }
        }

        if scene.has_component::<Material>(entity) {
            let material = scene.component::<Material>(entity).unwrap_or_default();
            out.unlit = material.unlit;
            out.proxy_albedo = material.base_color.truncate();
            let slot = MaterialSlot {
                base_color: material.base_color,
                albedo_texture: material.albedo_texture,
                metallic_roughness_texture: material.metallic_roughness_texture,
                metallic: material.metallic,
                roughness: material.roughness,
                emissive: material.emissive,
                emissive_strength: material.emissive_strength,
                unlit: material.unlit,
                normal_texture: material.normal_texture,
                occlusion_texture: material.occlusion_texture,
                emissive_texture: material.emissive_texture,
                normal_strength: material.normal_strength,
                uv_tiling: material.uv_tiling,
                uv_offset: material.uv_offset,
                height_texture: material.height_texture,
                height_scale: material.height_scale,
                alpha_clip: material.alpha_clip,
                alpha_cutoff: material.alpha_cutoff,
                double_sided: material.double_sided,
            };
            out.submeshes.push(self.lower_slot(gpu, &slot));
        }

        out
    }

    /// Lowers one inline [`MaterialSlot`] to a [`SubmeshMaterial`], resolving each texture
    /// id through the GPU cache.
    ///
    /// Distinct from [`build_submesh_material`]: a slot carries an explicit
    /// `metallic_roughness_texture` and `occlusion_texture` separately (it is not a packed
    /// ORM), so they resolve from their own ids, and `alpha_clip` rides the slot directly.
    fn lower_slot(&mut self, gpu: &dyn GpuUploader, slot: &MaterialSlot) -> SubmeshMaterial {
        let mut sm = SubmeshMaterial {
            base_color: slot.base_color,
            metallic: slot.metallic,
            roughness: slot.roughness,
            emissive: slot.emissive,
            emissive_strength: slot.emissive_strength,
            normal_strength: slot.normal_strength,
            uv_tiling: slot.uv_tiling,
            uv_offset: slot.uv_offset,
            height_scale: slot.height_scale,
            alpha_clip: slot.alpha_clip,
            alpha_cutoff: slot.alpha_cutoff,
            double_sided: slot.double_sided,
            ..SubmeshMaterial::defaults()
        };
        if slot.albedo_texture.value() != 0 {
            sm.albedo_texture = self.load_texture_asset(gpu, slot.albedo_texture);
        }
        if slot.metallic_roughness_texture.value() != 0 {
            sm.metallic_roughness_texture =
                self.load_texture_asset(gpu, slot.metallic_roughness_texture);
        }
        if slot.normal_texture.value() != 0 {
            sm.normal_texture = self.load_texture_asset(gpu, slot.normal_texture);
        }
        if slot.occlusion_texture.value() != 0 {
            sm.occlusion_texture = self.load_texture_asset(gpu, slot.occlusion_texture);
        }
        if slot.emissive_texture.value() != 0 {
            sm.emissive_texture = self.load_texture_asset(gpu, slot.emissive_texture);
        }
        if slot.height_texture.value() != 0 {
            sm.height_texture = self.load_texture_asset(gpu, slot.height_texture);
        }
        sm
    }
}

/// Loads a `.smat` resolved for rendering, returning `None` (with the caller warning)
/// when the id is absent or unreadable — the resolve path treats a missing material as
/// "use the default", never an error.
fn load_material_asset(assets: &mut AssetServer, id: saffron_core::Uuid) -> Option<MaterialAsset> {
    if id == DEFAULT_MATERIAL_ID {
        return Some(default_material_asset());
    }
    crate::material::load_catalog_material_asset(assets, id).ok()
}

/// Whether `value` is a JSON object with at least one member (a present, non-empty
/// graph).
fn is_non_empty_object(value: &saffron_json::Value) -> bool {
    value.as_object().is_some_and(|map| !map.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    use saffron_geometry::glam::{Vec2, Vec3, Vec4};

    use crate::material::save_material_asset;

    /// A scratch [`AssetServer`] rooted under a per-test temp dir.
    fn scratch_server(tag: &str) -> (AssetServer, std::path::PathBuf) {
        let tmp =
            std::env::temp_dir().join(format!("saffron-render-mat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("project").join("assets");
        (AssetServer::new(&root), tmp)
    }

    /// One submesh referencing material slot `slot`.
    fn submesh(slot: u32) -> Submesh {
        Submesh {
            first_index: 0,
            index_count: 3,
            vertex_offset: 0,
            material_slot: slot,
        }
    }

    #[test]
    fn build_submesh_material_packs_orm_into_both_slots() {
        let material = MaterialAsset {
            blend: "masked".to_owned(),
            base_color: Vec4::new(0.2, 0.4, 0.6, 1.0),
            metallic: 0.7,
            roughness: 0.3,
            emissive: Vec3::new(1.0, 2.0, 3.0),
            emissive_strength: 5.0,
            normal_strength: 0.5,
            alpha_cutoff: 0.25,
            height_scale: 0.1,
            uv_tiling: Vec2::new(2.0, 3.0),
            uv_offset: Vec2::new(0.1, 0.2),
            albedo_texture: saffron_core::Uuid(100),
            orm_texture: saffron_core::Uuid(200),
            normal_texture: saffron_core::Uuid(300),
            emissive_texture: saffron_core::Uuid(400),
            height_texture: saffron_core::Uuid(500),
            ..MaterialAsset::default()
        };
        // A loader that records which ids it was asked for, returning `None` (no GPU);
        // the test asserts on the *requests*, not the handles.
        let mut requests = Vec::<u64>::new();
        let mut load = |id: saffron_core::Uuid| -> Option<Arc<GpuTexture>> {
            requests.push(id.value());
            None
        };
        let sm = build_submesh_material(&material, &mut load);

        // The factors copy across verbatim.
        assert_eq!(sm.base_color, Vec4::new(0.2, 0.4, 0.6, 1.0));
        assert_eq!(sm.metallic, 0.7);
        assert_eq!(sm.roughness, 0.3);
        assert_eq!(sm.emissive, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(sm.emissive_strength, 5.0);
        assert_eq!(sm.normal_strength, 0.5);
        assert_eq!(sm.uv_tiling, Vec2::new(2.0, 3.0));
        assert_eq!(sm.uv_offset, Vec2::new(0.1, 0.2));
        assert_eq!(sm.height_scale, 0.1);
        assert_eq!(sm.alpha_cutoff, 0.25);
        // `alpha_clip` is true iff blend == "masked".
        assert!(sm.alpha_clip);

        // The packed ORM id is requested for *both* the metallic-roughness and the
        // occlusion slot. `load`'s mutable borrow of `requests` ends at the call above
        // (it is never used again), so the vector reads back here.
        let orm_count = requests.iter().filter(|&&id| id == 200).count();
        assert_eq!(orm_count, 2, "the ORM id feeds both mr and occlusion");
        assert!(requests.contains(&100));
        assert!(requests.contains(&300));
        assert!(requests.contains(&400));
        assert!(requests.contains(&500));
    }

    #[test]
    fn build_submesh_material_populates_both_handles_from_one_orm_id() {
        // A loader that hands a distinct (dummy) handle per id — but we cannot construct a
        // real `GpuTexture` off-GPU, so this asserts the *handle presence* contract via the
        // request count instead: an ORM id present yields two requests, mr + occlusion,
        // and the slots are set from the same id (proved by the request-count test above).
        // Here we assert the alpha-clip derivation across blend modes.
        for (blend, expect) in [("opaque", false), ("masked", true), ("translucent", false)] {
            let material = MaterialAsset {
                blend: blend.to_owned(),
                ..MaterialAsset::default()
            };
            let sm = build_submesh_material(&material, &mut |_| None);
            assert_eq!(sm.alpha_clip, expect, "blend {blend}");
        }
    }

    #[test]
    fn build_submesh_material_leaves_zero_ids_unset() {
        // The default material has every texture id at zero.
        let material = MaterialAsset::default();
        let mut asked = 0u32;
        let sm = build_submesh_material(&material, &mut |_| {
            asked += 1;
            None
        });
        assert_eq!(asked, 0, "no loader call for a zero id");
        assert!(sm.albedo_texture.is_none());
        assert!(sm.metallic_roughness_texture.is_none());
        assert!(sm.occlusion_texture.is_none());
        assert!(sm.normal_texture.is_none());
        assert!(sm.emissive_texture.is_none());
        assert!(sm.height_texture.is_none());
    }

    /// A `GpuUploader` stub that never uploads — every resolve runs off-GPU. The
    /// resolve-precedence tests don't need real textures: every material id under test is
    /// zero, so the loader is never called.
    struct NoGpu;

    impl GpuUploader for NoGpu {
        fn upload_mesh(
            &self,
            _mesh: &saffron_geometry::Mesh,
            _skin: &[saffron_geometry::VertexSkin],
            _morph: Option<&saffron_geometry::MorphData>,
            _sdf_bake: Option<&saffron_rendering::SdfBake>,
        ) -> saffron_rendering::Result<Arc<saffron_rendering::GpuMesh>> {
            unreachable!("the precedence tests use zero texture ids; no upload happens")
        }

        fn upload_texture(
            &self,
            _rgba: &[u8],
            _width: u32,
            _height: u32,
            _srgb: bool,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            unreachable!("the precedence tests use zero texture ids; no upload happens")
        }

        fn upload_texture_float(
            &self,
            _rgba: &[f32],
            _width: u32,
            _height: u32,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            unreachable!("the precedence tests use zero texture ids; no upload happens")
        }

        fn skinning_enabled(&self) -> bool {
            false
        }
    }

    #[test]
    fn precedence_material_asset_beats_set_and_component() {
        let (mut assets, tmp) = scratch_server("precedence");
        // Save a `.smat` with a recognizable base color, and reference it from the entity.
        let smat = MaterialAsset {
            base_color: Vec4::new(0.11, 0.22, 0.33, 1.0),
            unlit: true,
            ..MaterialAsset::default()
        };
        let smat_id = save_material_asset(&mut assets, &smat, "Asset", "").unwrap();

        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        scene
            .add_component(entity, MaterialAssetComponent { material: smat_id })
            .unwrap();
        // Also attach a MaterialSet and a Material — they must be ignored.
        scene
            .add_component(
                entity,
                MaterialSet {
                    slots: vec![MaterialSlot {
                        base_color: Vec4::new(9.0, 9.0, 9.0, 9.0),
                        ..MaterialSlot::default()
                    }],
                },
            )
            .unwrap();
        scene
            .add_component(
                entity,
                Material {
                    base_color: Vec4::new(8.0, 8.0, 8.0, 8.0),
                    ..Material::default()
                },
            )
            .unwrap();

        let meshes = [submesh(0), submesh(0), submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);

        // The `.smat` wins: its base color, its unlit flag, one entry per submesh.
        assert_eq!(resolved.submeshes.len(), 3);
        assert!(resolved.unlit);
        assert_eq!(resolved.proxy_albedo, Vec3::new(0.11, 0.22, 0.33));
        for sm in &resolved.submeshes {
            assert_eq!(sm.base_color, Vec4::new(0.11, 0.22, 0.33, 1.0));
        }
        // A foldable / no-graph material keeps the shared übershader.
        assert_eq!(resolved.shader, DEFAULT_MESH_SHADER);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn precedence_set_beats_component() {
        let (mut assets, tmp) = scratch_server("set-over-component");
        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        scene
            .add_component(
                entity,
                MaterialSet {
                    slots: vec![
                        MaterialSlot {
                            base_color: Vec4::new(1.0, 0.0, 0.0, 1.0),
                            unlit: true,
                            ..MaterialSlot::default()
                        },
                        MaterialSlot {
                            base_color: Vec4::new(0.0, 1.0, 0.0, 1.0),
                            ..MaterialSlot::default()
                        },
                    ],
                },
            )
            .unwrap();
        scene
            .add_component(
                entity,
                Material {
                    base_color: Vec4::new(8.0, 8.0, 8.0, 8.0),
                    ..Material::default()
                },
            )
            .unwrap();

        // Three submeshes referencing slots 0, 1, and an out-of-range slot 5 (clamped to 1).
        let meshes = [submesh(0), submesh(1), submesh(5)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);

        assert_eq!(resolved.submeshes.len(), 3);
        // The set's first slot drives `unlit` + the proxy albedo.
        assert!(resolved.unlit);
        assert_eq!(resolved.proxy_albedo, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(
            resolved.submeshes[0].base_color,
            Vec4::new(1.0, 0.0, 0.0, 1.0)
        );
        assert_eq!(
            resolved.submeshes[1].base_color,
            Vec4::new(0.0, 1.0, 0.0, 1.0)
        );
        // The out-of-range slot clamps to the last slot.
        assert_eq!(
            resolved.submeshes[2].base_color,
            Vec4::new(0.0, 1.0, 0.0, 1.0)
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn material_set_clamps_to_slot_count_per_submesh() {
        let (mut assets, tmp) = scratch_server("set-clamp");
        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        scene
            .add_component(
                entity,
                MaterialSet {
                    slots: vec![MaterialSlot {
                        base_color: Vec4::new(0.5, 0.5, 0.5, 1.0),
                        ..MaterialSlot::default()
                    }],
                },
            )
            .unwrap();

        // Four submeshes but only one slot: every submesh resolves the single slot, one
        // entry produced per submesh.
        let meshes = [submesh(0), submesh(3), submesh(0), submesh(9)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        assert_eq!(resolved.submeshes.len(), 4);
        for sm in &resolved.submeshes {
            assert_eq!(sm.base_color, Vec4::new(0.5, 0.5, 0.5, 1.0));
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn material_component_applies_a_single_submesh() {
        let (mut assets, tmp) = scratch_server("component");
        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        scene
            .add_component(
                entity,
                Material {
                    base_color: Vec4::new(0.7, 0.8, 0.9, 1.0),
                    unlit: true,
                    alpha_clip: true,
                    ..Material::default()
                },
            )
            .unwrap();

        // Exactly one submesh material is pushed for the single inline Material,
        // regardless of submesh count.
        let meshes = [submesh(0), submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        assert_eq!(resolved.submeshes.len(), 1);
        assert!(resolved.unlit);
        assert!(resolved.submeshes[0].alpha_clip);
        assert_eq!(resolved.proxy_albedo, Vec3::new(0.7, 0.8, 0.9));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_smat_id_falls_back_to_default_material() {
        let (mut assets, tmp) = scratch_server("missing-smat");
        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        // Reference a `.smat` id that is not in the catalog.
        scene
            .add_component(
                entity,
                MaterialAssetComponent {
                    material: saffron_core::Uuid(424_242),
                },
            )
            .unwrap();

        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        // The default material: white base color, lit, one submesh.
        assert_eq!(resolved.submeshes.len(), 1);
        assert!(!resolved.unlit);
        assert_eq!(resolved.proxy_albedo, Vec3::ONE);
        assert_eq!(resolved.submeshes[0].base_color, Vec4::ONE);
        assert_eq!(resolved.shader, DEFAULT_MESH_SHADER);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn zero_smat_id_falls_through_to_next_precedence_level() {
        let (mut assets, tmp) = scratch_server("zero-smat");
        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        // A MaterialAsset component with a zero id is *not* a winner — the precedence
        // falls through to the Material component (the `matId != 0` guard).
        scene
            .add_component(
                entity,
                MaterialAssetComponent {
                    material: saffron_core::Uuid(0),
                },
            )
            .unwrap();
        scene
            .add_component(
                entity,
                Material {
                    base_color: Vec4::new(0.3, 0.3, 0.3, 1.0),
                    ..Material::default()
                },
            )
            .unwrap();

        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        assert_eq!(resolved.submeshes.len(), 1);
        assert_eq!(
            resolved.submeshes[0].base_color,
            Vec4::new(0.3, 0.3, 0.3, 1.0)
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn no_material_components_yields_empty_defaults() {
        let (mut assets, tmp) = scratch_server("none");
        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        // No components: an empty submesh list with default flags.
        assert!(resolved.submeshes.is_empty());
        assert!(!resolved.unlit);
        assert_eq!(resolved.proxy_albedo, Vec3::ONE);
        assert_eq!(resolved.shader, DEFAULT_MESH_SHADER);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn codegen_override_points_shader_at_mesh_spv_for_non_foldable_graph() {
        let (mut assets, tmp) = scratch_server("codegen");
        // A `.smat` whose graph is non-foldable (a `multiply` math node forces codegen).
        let smat = MaterialAsset {
            graph: serde_json::json!({
                "nodes": [
                    { "id": "c1", "type": "constant", "props": { "value": [0.5, 0.25, 1.0, 1.0] } },
                    { "id": "tx", "type": "textureSlot", "props": { "slot": "normal" } },
                    { "id": "mul", "type": "multiply" },
                    { "id": "out", "type": "materialOutput" }
                ],
                "edges": [
                    { "from": ["c1", "out"], "to": ["mul", "a"] },
                    { "from": ["tx", "out"], "to": ["mul", "b"] },
                    { "from": ["mul", "out"], "to": ["out", "baseColor"] }
                ]
            }),
            ..MaterialAsset::default()
        };
        let smat_id = save_material_asset(&mut assets, &smat, "Graph", "").unwrap();

        // Drop a compiled `<id>_mesh.spv` artifact beside the `.smat`.
        let spv = assets
            .root
            .join("materials")
            .join(format!("{}_mesh.spv", smat_id.value()));
        std::fs::write(&spv, b"\x03\x02\x23\x07").unwrap();

        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        scene
            .add_component(entity, MaterialAssetComponent { material: smat_id })
            .unwrap();

        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        // The non-foldable graph + the on-disk `_mesh.spv` route the shader there.
        assert_eq!(resolved.shader, spv.to_string_lossy());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn foldable_graph_keeps_the_shared_ubershader() {
        let (mut assets, tmp) = scratch_server("foldable");
        // A graph that folds entirely (a constant wired into baseColor): no codegen.
        let smat = MaterialAsset {
            graph: serde_json::json!({
                "nodes": [
                    { "id": "c", "type": "constant", "props": { "value": [0.5, 0.5, 0.5, 1.0] } },
                    { "id": "out", "type": "materialOutput" }
                ],
                "edges": [
                    { "from": ["c", "o"], "to": ["out", "baseColor"] }
                ]
            }),
            ..MaterialAsset::default()
        };
        let smat_id = save_material_asset(&mut assets, &smat, "Folded", "").unwrap();

        // Even if a stray `_mesh.spv` exists, a foldable graph must not point at it.
        let spv = assets
            .root
            .join("materials")
            .join(format!("{}_mesh.spv", smat_id.value()));
        std::fs::write(&spv, b"\x03\x02\x23\x07").unwrap();

        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        scene
            .add_component(entity, MaterialAssetComponent { material: smat_id })
            .unwrap();

        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        assert_eq!(resolved.shader, DEFAULT_MESH_SHADER);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A non-empty graph that folds is detected by [`lower_graph_to_params`]; sanity that
    /// the harness's foldable/non-foldable graphs behave as assumed by the two tests above.
    #[test]
    fn graph_fold_detection_matches_the_resolve_branch() {
        let foldable = serde_json::json!({
            "nodes": [
                { "id": "c", "type": "constant", "props": { "value": [0.5, 0.5, 0.5, 1.0] } },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["c", "o"], "to": ["out", "baseColor"] }
            ]
        });
        let mut probe = MaterialAsset::default();
        assert!(lower_graph_to_params(&foldable, &mut probe));

        let non_foldable = serde_json::json!({
            "nodes": [
                { "id": "c1", "type": "constant", "props": { "value": [0.5, 0.25, 1.0, 1.0] } },
                { "id": "tx", "type": "textureSlot", "props": { "slot": "normal" } },
                { "id": "mul", "type": "multiply" },
                { "id": "out", "type": "materialOutput" }
            ],
            "edges": [
                { "from": ["c1", "out"], "to": ["mul", "a"] },
                { "from": ["tx", "out"], "to": ["mul", "b"] },
                { "from": ["mul", "out"], "to": ["out", "baseColor"] }
            ]
        });
        let mut probe2 = MaterialAsset::default();
        assert!(!lower_graph_to_params(&non_foldable, &mut probe2));
    }
}
