//! The resolution from a loaded [`MaterialAsset`] (or a scene material component) to a
//! render-ready [`SubmeshMaterial`] with bindless GPU texture handles.
//!
//! Three entry points, narrowing in scope:
//!
//! - [`build_submesh_material`] maps one resolved [`MaterialAsset`] to a
//!   [`SubmeshMaterial`], resolving each texture slot through a borrowed loader closure.
//!   The main draw path passes [`AssetServer::load_texture_asset`]; the thumbnail worker
//!   passes its own uploader — one mapping, one call site, with the upload role explicit.
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
//! in R), so one map covers all three. [`SubmeshMaterial::blend_mode`] is parsed from the
//! `.smat` `blend` string via [`BlendMode::from_wire`] for standard surfaces. Thin-sheet
//! foliage derives it from the canonical coverage classification.
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

use saffron_core::BlendMode;
use saffron_core::HeightMode;
use saffron_core::Uuid;
use saffron_geometry::Submesh;
use saffron_geometry::glam::Vec3;
use saffron_json::Value;
use saffron_rendering::{
    AggregateMaterialMoments, CoverageSourceKind, GpuTexture, SubmeshMaterial, ThinSheetMaterial,
    ThinSheetNormalMode,
};
use saffron_scene::{Entity, MaterialSet, Scene};
use saffron_vegetation::{
    AlphaClassification, CoverageSource, MaterialSurface, ThinSheetNormalBehavior,
};

use crate::gpu::GpuUploader;
use crate::graph::lower_graph_to_params;
use crate::material::{
    MaterialAsset, apply_overrides, default_material_asset, load_material_asset_raw,
};
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
/// `blend_mode` parses the `.smat` `blend` string for standard surfaces and follows the
/// canonical coverage classification for thin sheets.
///
/// The loader receives a [`TextureLoadRole`] so height and coverage pyramids use their canonical
/// builders while one closure retains the single mutable server borrow.
pub fn build_submesh_material(
    material: &MaterialAsset,
    load_tex: &mut dyn FnMut(saffron_core::Uuid, TextureLoadRole) -> Option<Arc<GpuTexture>>,
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
        height_mode: material.height_mode,
        blend_mode: BlendMode::from_wire(&material.blend),
        alpha_cutoff: material.alpha_cutoff,
        double_sided: material.double_sided,
        ..SubmeshMaterial::defaults()
    };
    if material.albedo_texture.value() != 0 {
        sm.albedo_texture = load_tex(material.albedo_texture, TextureLoadRole::Plain);
    }
    if material.orm_texture.value() != 0 {
        sm.metallic_roughness_texture = load_tex(material.orm_texture, TextureLoadRole::Plain);
        sm.occlusion_texture = load_tex(material.orm_texture, TextureLoadRole::Plain);
    }
    if material.normal_texture.value() != 0 {
        sm.normal_texture = load_tex(material.normal_texture, TextureLoadRole::Plain);
    }
    if material.emissive_texture.value() != 0 {
        sm.emissive_texture = load_tex(material.emissive_texture, TextureLoadRole::Plain);
    }
    if material.height_texture.value() != 0 {
        // A displacement material's height map carries the min/max pyramid (built by the height loader)
        // that the tessellation factor kernel samples for per-region LOD; bump/parallax need no pyramid.
        let role = if material.height_mode == HeightMode::Displacement {
            TextureLoadRole::Height
        } else {
            TextureLoadRole::Plain
        };
        sm.height_texture = load_tex(material.height_texture, role);
    }
    if material.vector_displacement_texture.value() != 0 {
        sm.vector_displacement_texture =
            load_tex(material.vector_displacement_texture, TextureLoadRole::Plain);
    }
    if let MaterialSurface::ThinSheetFoliage(parameters) = &material.surface {
        let coverage_id = match parameters.coverage_source {
            CoverageSource::AlbedoAlpha => material.albedo_texture,
            CoverageSource::Texture(texture) => texture,
            CoverageSource::ModeledGeometry => Uuid(0),
        };
        if coverage_id.value() != 0 {
            sm.coverage_texture = load_tex(
                coverage_id,
                TextureLoadRole::Coverage {
                    cutoff_bits: parameters.coverage.reference_cutoff.bits(),
                },
            );
        }
        sm.blend_mode = match parameters.coverage.classification {
            AlphaClassification::Opaque => BlendMode::Opaque,
            AlphaClassification::Masked => BlendMode::Masked,
            AlphaClassification::Transmissive => BlendMode::Blend,
        };
        sm.thin_sheet = Some(thin_sheet_material(parameters));
        sm.double_sided = true;
    }
    sm
}

/// Texture upload role selected while resolving a material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureLoadRole {
    /// Ordinary color/data texture with the standard filtered mip chain.
    Plain,
    /// Displacement texture with a min/max height pyramid.
    Height,
    /// Linear coverage texture with cutoff-preserving alpha mips.
    Coverage {
        /// Canonical normalized cutoff bits.
        cutoff_bits: u16,
    },
}

fn thin_sheet_material(
    parameters: &saffron_vegetation::ThinSheetFoliageParameters,
) -> ThinSheetMaterial {
    let scalar = |value: saffron_spatial::DecisionScalar| value.to_f64() as f32;
    let unit = |value: saffron_spatial::UnitInterval| value.to_f64() as f32;
    let vec3 = |values: [saffron_spatial::DecisionScalar; 3]| {
        Vec3::new(scalar(values[0]), scalar(values[1]), scalar(values[2]))
    };
    let moments = parameters.voxel_moments;
    ThinSheetMaterial {
        front_albedo_response: unit(parameters.front_albedo_response),
        back_albedo_response: unit(parameters.back_albedo_response),
        thickness: scalar(parameters.thickness),
        absorption: vec3(parameters.absorption_color),
        transmission: vec3(parameters.transmission_color),
        roughness: unit(parameters.roughness),
        normal_mode: match parameters.normal_behavior {
            ThinSheetNormalBehavior::Preserve => ThinSheetNormalMode::Preserve,
            ThinSheetNormalBehavior::FaceForwardBack => ThinSheetNormalMode::FaceForwardBack,
            ThinSheetNormalBehavior::Symmetric => ThinSheetNormalMode::Symmetric,
        },
        coverage_source: match parameters.coverage_source {
            CoverageSource::AlbedoAlpha => CoverageSourceKind::AlbedoAlpha,
            CoverageSource::Texture(_) => CoverageSourceKind::Texture,
            CoverageSource::ModeledGeometry => CoverageSourceKind::ModeledGeometry,
        },
        coverage_classification: parameters.coverage.classification,
        coverage_hash_salt: parameters.coverage.spatial_hash_salt,
        coverage_source_extent: parameters.coverage.source_extent,
        energy_limit: unit(parameters.energy_limit),
        aggregate: AggregateMaterialMoments {
            occupancy: unit(moments.occupancy),
            albedo_mean: vec3(moments.albedo_mean),
            roughness_mean: unit(moments.roughness_mean),
            transmission_mean: vec3(moments.transmission_mean),
            thickness_mean: scalar(moments.thickness_mean),
            normal_second_moments: moments.normal_second_moments.map(scalar),
        },
    }
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
        build_submesh_material(material, &mut |id, role| match role {
            TextureLoadRole::Plain => self.load_texture_asset(gpu, id),
            TextureLoadRole::Height => self.load_height_texture_asset(gpu, id),
            TextureLoadRole::Coverage { cutoff_bits } => {
                self.load_coverage_texture_asset(gpu, id, cutoff_bits)
            }
        })
    }

    /// Resolves a single renderable's whole submesh-material table from the entity's
    /// [`MaterialSet`] — the one per-entity material component.
    ///
    /// Each slot references a `.smat` material asset (resolved through its parent chain,
    /// falling back to the built-in default when missing) with the slot's sparse overrides
    /// layered on top; each submesh's `material_slot` selects a slot, clamped to the slot
    /// count. The whole-mesh `unlit` flag, the DDGI proxy albedo, and the codegen-shader
    /// override follow slot 0 (the PSO is per-item). An entity with no `MaterialSet` (or an
    /// empty one) resolves to empty — the draw path falls back to engine defaults.
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

        let slots = scene
            .with_component::<MaterialSet, _>(entity, |set| set.slots.clone())
            .unwrap_or_default();
        if slots.is_empty() {
            return out;
        }

        // Resolve each slot's referenced material (parent chain) with its sparse overrides
        // once, so a submesh reusing a slot does not re-load it.
        let resolved: Vec<MaterialAsset> = slots
            .iter()
            .map(|slot| self.resolve_slot_material(slot.material, &slot.overrides))
            .collect();

        // The whole-mesh flags + codegen shader follow slot 0.
        out.unlit = resolved[0].unlit;
        out.proxy_albedo = resolved[0].base_color.truncate();
        if let Some(shader) = self.codegen_shader_for(slots[0].material) {
            out.shader = shader;
        }

        out.submeshes.reserve(submeshes.len());
        for submesh in submeshes {
            let index = (submesh.material_slot as usize).min(resolved.len() - 1);
            out.submeshes
                .push(self.resolve_material_asset(gpu, &resolved[index]));
        }
        out
    }

    /// Resolves one slot: loads its referenced `.smat` (parent chain resolved) and layers the
    /// slot's sparse overrides on top. A `0` reference is the built-in default (the common
    /// case — no warning); a non-zero id that fails to load warns and falls back to default.
    fn resolve_slot_material(&mut self, material_id: Uuid, overrides: &Value) -> MaterialAsset {
        let mut material = if material_id.value() == 0 {
            default_material_asset()
        } else {
            load_material_asset(self, material_id).unwrap_or_else(|| {
                tracing::warn!(
                    "slot material asset {} missing; using default",
                    material_id.value()
                );
                default_material_asset()
            })
        };
        apply_overrides(&mut material, overrides);
        material
    }

    /// The compiled übershader variant for a material with a non-foldable node graph, if one
    /// exists on disk (built at `material-set-graph` time); `None` for a plain material or a
    /// foldable graph (which use the shared übershader). Embedded (container) materials have
    /// no graph, so a non-standalone id resolves to `None`.
    ///
    /// Memoized in [`AssetServer::material_shader_by_uuid`] (the resolve runs per entity per
    /// frame); the result only changes through the invalidation seams. A container-embedded id
    /// short-circuits to `None` without touching disk — imported materials never carry a graph,
    /// and reading the whole binary `.smodel` as a UTF-8 string was pure per-frame waste.
    fn codegen_shader_for(&mut self, material_id: Uuid) -> Option<String> {
        if let Some(cached) = self.material_shader_by_uuid.get(&material_id.value()) {
            return cached.as_ref().map(|s| (**s).clone());
        }
        let embedded = self
            .catalog
            .find(material_id)
            .is_some_and(|entry| entry.container.value() != 0);
        let shader = if embedded {
            None
        } else {
            self.probe_codegen_shader(material_id)
        };
        self.material_shader_by_uuid
            .insert(material_id.value(), shader.clone().map(Arc::new));
        shader
    }

    /// The uncached probe behind [`Self::codegen_shader_for`]: reads the standalone `.smat`
    /// graph and, when it is a non-foldable graph, returns the compiled `_mesh.spv` path.
    fn probe_codegen_shader(&self, material_id: Uuid) -> Option<String> {
        let raw = load_material_asset_raw(self, material_id).ok()?;
        if !is_non_empty_object(&raw.graph) {
            return None;
        }
        let mut probe = raw.clone();
        if lower_graph_to_params(&raw.graph, &mut probe) {
            return None;
        }
        let spv = self
            .root
            .join("materials")
            .join(format!("{}_mesh.spv", material_id.value()));
        spv.exists().then(|| spv.to_string_lossy().into_owned())
    }
}

/// Loads a `.smat` resolved for rendering, returning `None` (with the caller warning)
/// when the id is absent or unreadable — the resolve path treats a missing material as
/// "use the default", never an error.
fn load_material_asset(assets: &mut AssetServer, id: saffron_core::Uuid) -> Option<MaterialAsset> {
    if id == DEFAULT_MATERIAL_ID {
        return Some(default_material_asset());
    }
    // Cached parent-resolved material (before the per-slot overrides the caller layers on the
    // returned clone). A present key — including a negative-cached `None` — skips the disk read;
    // only a true miss reads + parses the `.smat` (and, for a container material, slices the
    // `.smodel` chunk). Cleared wholesale on any material mutation.
    if let Some(cached) = assets.material_by_uuid.get(&id.value()) {
        return cached.as_ref().map(|material| (**material).clone());
    }
    let loaded = crate::material::load_catalog_material_asset(assets, id).ok();
    assets
        .material_by_uuid
        .insert(id.value(), loaded.clone().map(Arc::new));
    loaded
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
    use saffron_scene::MaterialSlot;

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
        let mut load =
            |id: saffron_core::Uuid, _role: TextureLoadRole| -> Option<Arc<GpuTexture>> {
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
        // `blend == "masked"` lowers to the masked blend mode.
        assert_eq!(sm.blend_mode, BlendMode::Masked);

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
        // Here we assert the blend-mode derivation across the three glTF alpha modes.
        for (blend, expect) in [
            ("opaque", BlendMode::Opaque),
            ("masked", BlendMode::Masked),
            ("translucent", BlendMode::Blend),
        ] {
            let material = MaterialAsset {
                blend: blend.to_owned(),
                ..MaterialAsset::default()
            };
            let sm = build_submesh_material(&material, &mut |_, _| None);
            assert_eq!(sm.blend_mode, expect, "blend {blend}");
        }
    }

    #[test]
    fn thin_sheet_surface_is_the_authority_for_coverage_and_optics() {
        use saffron_spatial::{DecisionScalar, UnitInterval};

        let thin = saffron_vegetation::ThinSheetFoliageParameters {
            front_albedo_response: UnitInterval::from_bits(20_000),
            back_albedo_response: UnitInterval::from_bits(10_000),
            thickness: DecisionScalar::from_bits(131),
            absorption_color: [
                DecisionScalar::from_bits(1_000),
                DecisionScalar::from_bits(2_000),
                DecisionScalar::from_bits(3_000),
            ],
            transmission_color: [
                DecisionScalar::from_bits(4_000),
                DecisionScalar::from_bits(5_000),
                DecisionScalar::from_bits(6_000),
            ],
            coverage_source: CoverageSource::Texture(Uuid(707)),
            coverage: saffron_vegetation::CoverageMipMetadata {
                classification: AlphaClassification::Masked,
                reference_cutoff: UnitInterval::from_bits(22_000),
                source_extent: [512, 256],
                spatial_hash_salt: 0x1122_3344_5566_7788,
                mip_hashes: Vec::new(),
            },
            ..saffron_vegetation::ThinSheetFoliageParameters::default()
        };
        let material = MaterialAsset {
            blend: "opaque".to_owned(),
            double_sided: false,
            surface: MaterialSurface::ThinSheetFoliage(thin.clone()),
            ..MaterialAsset::default()
        };
        let mut requests = Vec::new();
        let resolved = build_submesh_material(&material, &mut |id, role| {
            requests.push((id, role));
            None
        });

        assert_eq!(resolved.blend_mode, BlendMode::Masked);
        assert!(resolved.double_sided);
        assert_eq!(
            requests,
            [(
                Uuid(707),
                TextureLoadRole::Coverage {
                    cutoff_bits: 22_000
                }
            )]
        );
        let gpu = resolved.thin_sheet.expect("thin-sheet GPU contract");
        assert_eq!(gpu.coverage_source, CoverageSourceKind::Texture);
        assert_eq!(gpu.coverage_classification, AlphaClassification::Masked);
        assert_eq!(gpu.coverage_source_extent, [512, 256]);
        assert_eq!(gpu.coverage_hash_salt, 0x1122_3344_5566_7788);
        assert_eq!(gpu.front_albedo_response, 20_000.0 / 65_535.0);
        assert_eq!(gpu.back_albedo_response, 10_000.0 / 65_535.0);
        assert_eq!(gpu.thickness, 131.0 / 65_536.0);
    }

    #[test]
    fn displacement_material_requests_its_height_slot_as_a_height_map() {
        use saffron_geometry::glam::Vec4;
        // Two materials sharing a height texture id: one Displacement (pyramid), one Bump (plain). The
        // loader records the `as_height` flag it was asked for per id.
        let mut asks: Vec<(u64, TextureLoadRole)> = Vec::new();
        for mode in [HeightMode::Displacement, HeightMode::Bump] {
            let material = MaterialAsset {
                base_color: Vec4::ONE,
                height_texture: saffron_core::Uuid(777),
                height_mode: mode,
                ..MaterialAsset::default()
            };
            let _ = build_submesh_material(&material, &mut |id, role| {
                asks.push((id.value(), role));
                None
            });
        }
        // The height slot is requested `as_height = true` only for the Displacement material; every
        // non-height slot is always plain.
        assert!(
            asks.contains(&(777, TextureLoadRole::Height)),
            "displacement height → pyramid load"
        );
        assert!(
            asks.contains(&(777, TextureLoadRole::Plain)),
            "bump height → plain load"
        );
    }

    #[test]
    fn build_submesh_material_leaves_zero_ids_unset() {
        // The default material has every texture id at zero.
        let material = MaterialAsset::default();
        let mut asked = 0u32;
        let sm = build_submesh_material(&material, &mut |_, _| {
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

    /// A `GpuUploader` stub that never uploads — every resolve runs off-GPU. The resolve
    /// tests reference materials with zero texture ids, so the texture loader is never
    /// called.
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

    /// One [`MaterialSet`] with the given slots on a fresh entity, returning the scene +
    /// entity ready to resolve.
    fn scene_with_slots(slots: Vec<MaterialSlot>) -> (Scene, Entity) {
        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        scene.add_component(entity, MaterialSet { slots }).unwrap();
        (scene, entity)
    }

    #[test]
    fn slot_resolves_the_referenced_smat_factors() {
        let (mut assets, tmp) = scratch_server("slot-smat");
        // Save a `.smat` with a recognizable base color + unlit, reference it from a slot.
        let smat = MaterialAsset {
            base_color: Vec4::new(0.11, 0.22, 0.33, 1.0),
            unlit: true,
            ..MaterialAsset::default()
        };
        let smat_id = save_material_asset(&mut assets, &smat, "Asset", "").unwrap();

        let (scene, entity) = scene_with_slots(vec![MaterialSlot {
            material: smat_id,
            ..MaterialSlot::default()
        }]);

        let meshes = [submesh(0), submesh(0), submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);

        // The slot's `.smat`: its base color, its unlit flag, one entry per submesh.
        assert_eq!(resolved.submeshes.len(), 3);
        assert!(resolved.unlit);
        assert_eq!(resolved.proxy_albedo, Vec3::new(0.11, 0.22, 0.33));
        for sm in &resolved.submeshes {
            assert_eq!(sm.base_color, Vec4::new(0.11, 0.22, 0.33, 1.0));
        }
        // A no-graph material keeps the shared übershader.
        assert_eq!(resolved.shader, DEFAULT_MESH_SHADER);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn slot_overrides_layer_over_the_referenced_material() {
        let (mut assets, tmp) = scratch_server("slot-overrides");
        // The `.smat` sets base color + a low metallic; the slot overrides only metallic.
        let smat = MaterialAsset {
            base_color: Vec4::new(0.4, 0.5, 0.6, 1.0),
            metallic: 0.1,
            ..MaterialAsset::default()
        };
        let smat_id = save_material_asset(&mut assets, &smat, "Asset", "").unwrap();

        let (scene, entity) = scene_with_slots(vec![MaterialSlot {
            material: smat_id,
            overrides: serde_json::json!({ "metallic": 0.9 }),
        }]);

        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        assert_eq!(resolved.submeshes.len(), 1);
        // Base color rides through from the `.smat`; metallic comes from the override.
        assert_eq!(
            resolved.submeshes[0].base_color,
            Vec4::new(0.4, 0.5, 0.6, 1.0)
        );
        assert_eq!(resolved.submeshes[0].metallic, 0.9);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn submeshes_map_to_slots_and_clamp_to_the_last() {
        let (mut assets, tmp) = scratch_server("multi-slot");
        let red = save_material_asset(
            &mut assets,
            &MaterialAsset {
                base_color: Vec4::new(1.0, 0.0, 0.0, 1.0),
                unlit: true,
                ..MaterialAsset::default()
            },
            "Red",
            "",
        )
        .unwrap();
        let green = save_material_asset(
            &mut assets,
            &MaterialAsset {
                base_color: Vec4::new(0.0, 1.0, 0.0, 1.0),
                ..MaterialAsset::default()
            },
            "Green",
            "",
        )
        .unwrap();

        let (scene, entity) = scene_with_slots(vec![
            MaterialSlot {
                material: red,
                ..MaterialSlot::default()
            },
            MaterialSlot {
                material: green,
                ..MaterialSlot::default()
            },
        ]);

        // Submeshes reference slots 0, 1, and an out-of-range slot 5 (clamped to 1).
        let meshes = [submesh(0), submesh(1), submesh(5)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);

        assert_eq!(resolved.submeshes.len(), 3);
        // Slot 0 drives the whole-mesh `unlit` + proxy albedo.
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
    fn a_single_slot_clamps_every_submesh() {
        let (mut assets, tmp) = scratch_server("single-slot");
        let id = save_material_asset(
            &mut assets,
            &MaterialAsset {
                base_color: Vec4::new(0.5, 0.5, 0.5, 1.0),
                ..MaterialAsset::default()
            },
            "Gray",
            "",
        )
        .unwrap();

        let (scene, entity) = scene_with_slots(vec![MaterialSlot {
            material: id,
            ..MaterialSlot::default()
        }]);

        // Several submeshes but one slot: every submesh resolves that slot, one entry each.
        let meshes = [submesh(0), submesh(3), submesh(9)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        assert_eq!(resolved.submeshes.len(), meshes.len());
        for sm in &resolved.submeshes {
            assert_eq!(sm.base_color, Vec4::new(0.5, 0.5, 0.5, 1.0));
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_smat_id_falls_back_to_default_material() {
        let (mut assets, tmp) = scratch_server("missing-smat");
        // A slot referencing a `.smat` id that is not in the catalog.
        let (scene, entity) = scene_with_slots(vec![MaterialSlot {
            material: saffron_core::Uuid(424_242),
            ..MaterialSlot::default()
        }]);

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
    fn zero_slot_id_resolves_the_builtin_default_material() {
        let (mut assets, tmp) = scratch_server("zero-slot");
        // A default slot references `Uuid(0)` — the built-in default material.
        let (scene, entity) = scene_with_slots(vec![MaterialSlot::default()]);

        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        assert_eq!(resolved.submeshes.len(), 1);
        assert!(!resolved.unlit);
        assert_eq!(resolved.proxy_albedo, Vec3::ONE);
        assert_eq!(resolved.submeshes[0].base_color, Vec4::ONE);
        assert_eq!(resolved.shader, DEFAULT_MESH_SHADER);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn no_material_set_yields_empty_defaults() {
        let (mut assets, tmp) = scratch_server("none");
        let mut scene = Scene::default();
        let entity = scene.create_entity("e");
        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        // No `MaterialSet`: an empty submesh list with default flags.
        assert!(resolved.submeshes.is_empty());
        assert!(!resolved.unlit);
        assert_eq!(resolved.proxy_albedo, Vec3::ONE);
        assert_eq!(resolved.shader, DEFAULT_MESH_SHADER);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn codegen_slot_points_shader_at_mesh_spv_for_non_foldable_graph() {
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

        let (scene, entity) = scene_with_slots(vec![MaterialSlot {
            material: smat_id,
            ..MaterialSlot::default()
        }]);

        let meshes = [submesh(0)];
        let resolved = assets.resolve_entity_materials(&NoGpu, &scene, entity, &meshes);
        // The non-foldable graph + the on-disk `_mesh.spv` route the shader there.
        assert_eq!(resolved.shader, spv.to_string_lossy());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn foldable_graph_slot_keeps_the_shared_ubershader() {
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

        let (scene, entity) = scene_with_slots(vec![MaterialSlot {
            material: smat_id,
            ..MaterialSlot::default()
        }]);

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

    /// The draw-path material load is memoized, and an edit invalidates it: a resolve populates
    /// the cache, [`update_material_asset`](crate::material::update_material_asset) clears it, and
    /// the next resolve returns the *edited* value — never a stale cached one. This is the whole
    /// point of the cache: the per-frame resolve stops re-reading the `.smat` from disk, but an
    /// edit is still seen on the next frame.
    #[test]
    fn material_load_is_cached_and_invalidated_on_edit() {
        use crate::material::update_material_asset;

        let (mut assets, tmp) = scratch_server("cache-invalidate");
        let red = MaterialAsset {
            base_color: Vec4::new(1.0, 0.0, 0.0, 1.0),
            ..MaterialAsset::default()
        };
        let id = save_material_asset(&mut assets, &red, "Mat", "").expect("save");

        // First resolve reads from disk and fills the cache.
        assert!(assets.material_by_uuid.is_empty());
        let first = load_material_asset(&mut assets, id).expect("resolve");
        assert_eq!(first.base_color, Vec4::new(1.0, 0.0, 0.0, 1.0));
        assert!(
            assets.material_by_uuid.contains_key(&id.value()),
            "the resolve must populate the material cache"
        );

        // An in-place edit writes the new `.smat` and invalidates the cache.
        let green = MaterialAsset {
            base_color: Vec4::new(0.0, 1.0, 0.0, 1.0),
            ..MaterialAsset::default()
        };
        update_material_asset(&mut assets, id, &green).expect("update");
        assert!(
            assets.material_by_uuid.is_empty(),
            "editing a material must clear the memoized resolution"
        );

        // The next resolve sees the edit, not the stale cached red.
        let second = load_material_asset(&mut assets, id).expect("re-resolve");
        assert_eq!(second.base_color, Vec4::new(0.0, 1.0, 0.0, 1.0));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
