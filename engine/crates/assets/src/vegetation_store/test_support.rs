//! Shared fixtures for the vegetation artifact-store tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use saffron_core::Uuid;
use saffron_geometry::{
    AppearanceError, HierarchyRepresentation, PortableBounds, PortableHierarchyNode,
    PortableHierarchyPage, PortableMaterialMoments, PortableRayTracingRecord,
    PortableVirtualHierarchy, PortableVoxelBrick, PortableVoxelVertex, VirtualMaterialClass,
};
use saffron_spatial::{DecisionScalar, WorldCellKey};
use saffron_vegetation::{
    ContentHash, CookDependency, CookDependencyAddress, CookGraph, CookNodeAddress, CookNodeRecord,
    CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate, PlantCompiledSection,
    PlantCompiledSectionKind, PlantTagId,
};

pub(super) fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    std::env::temp_dir().join(format!(
        "saffron-vegetation-cas-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

pub(super) fn cook_graph_bytes() -> Vec<u8> {
    let versions = CookVersionSet::current();
    let platform = CookPlatformProfile {
        target: "aarch64-apple-darwin".to_owned(),
        content_profile: "portable-vulkan".to_owned(),
        toolchain: "rust-1.96.0".to_owned(),
        features: vec!["canonical-fixed".to_owned()],
    };
    let mut node = CookNodeRecord {
        address: CookNodeAddress::Cell {
            map: Uuid(10),
            cell: WorldCellKey::base(-1, 2, 0),
        },
        cook_key: ContentHash::default(),
        output_hash: ContentHash::new([7; 32]),
        dependencies: vec![CookDependency {
            address: CookDependencyAddress::Contract {
                namespace: "test/schema".to_owned(),
            },
            content_hash: ContentHash::new([8; 32]),
            bounds: None,
            halo: DecisionScalar::from_bits(0),
            ancestor_level: None,
        }],
        estimate: CookWorkEstimate::default(),
        actual: CookWorkActual::default(),
    };
    node.cook_key = node.calculate_cook_key(versions, &platform).unwrap();
    CookGraph {
        versions,
        platform,
        nodes: vec![node],
    }
    .canonical_bytes()
    .unwrap()
}

fn part_table(tags: &[PlantTagId]) -> Vec<u8> {
    let domain = b"saffron-anima/splantc/part-table/v2";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u64::try_from(domain.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(&u64::try_from(tags.len()).unwrap().to_be_bytes());
    for tag in tags {
        bytes.extend_from_slice(&tag.value().to_be_bytes());
    }
    bytes
}

pub(super) fn plant_sections(tags: &[PlantTagId]) -> Vec<PlantCompiledSection> {
    let bounds = PortableBounds {
        min_bits: [-65_536; 3],
        max_bits: [65_536; 3],
    };
    let mut hierarchy = PortableVirtualHierarchy::default();
    hierarchy.voxel_bricks.push(PortableVoxelBrick {
        id: 0,
        dimensions: [8; 3],
        bounds,
        deformed_bounds: bounds,
        occupancy: vec![u8::MAX; 64],
        moments: PortableMaterialMoments::default(),
        material_class: VirtualMaterialClass::Opaque,
        opacity_micromap: false,
        vertices: vec![
            PortableVoxelVertex {
                position_bits: [-65_536, -65_536, 0],
                normal_oct: [0, 0],
            },
            PortableVoxelVertex {
                position_bits: [65_536, -65_536, 0],
                normal_oct: [0, 0],
            },
            PortableVoxelVertex {
                position_bits: [0, 65_536, 0],
                normal_oct: [0, 0],
            },
        ],
        indices: vec![0, 1, 2],
        page: 0,
        appearance_error: AppearanceError::default(),
    });
    hierarchy.nodes.push(PortableHierarchyNode {
        id: 0,
        representation: HierarchyRepresentation::Voxel { brick: 0 },
        parent: None,
        children: Vec::new(),
        page: 0,
        bounds,
        deformed_bounds: bounds,
        appearance_error: AppearanceError::default(),
    });
    hierarchy.pages.push(PortableHierarchyPage {
        id: 0,
        dependency: None,
        node: 0,
        bounds,
        deformed_bounds: bounds,
        transition_error: AppearanceError::default(),
        guaranteed_root: true,
    });
    hierarchy.roots.push(0);
    hierarchy.ray_tracing.push(PortableRayTracingRecord {
        node: 0,
        material_class: VirtualMaterialClass::Opaque,
        opacity_micromap: false,
        requires_any_hit: false,
    });
    PlantCompiledSectionKind::ALL
        .into_iter()
        .map(|kind| {
            let bytes = match kind {
                PlantCompiledSectionKind::PartTable => part_table(tags),
                PlantCompiledSectionKind::TriangleHierarchy => {
                    hierarchy.triangle_hierarchy_bytes().unwrap()
                }
                PlantCompiledSectionKind::VoxelHierarchy => {
                    hierarchy.voxel_hierarchy_bytes().unwrap()
                }
                PlantCompiledSectionKind::Deformation => hierarchy.deformation_bytes().unwrap(),
                PlantCompiledSectionKind::PageDirectory => {
                    hierarchy.page_directory_bytes().unwrap()
                }
                PlantCompiledSectionKind::RayTracing => hierarchy.ray_tracing_bytes().unwrap(),
                // The fixture family binds no coverage texture, so it packs no atlas: the layout
                // marker and the texture container are both absent.
                PlantCompiledSectionKind::TextureContainer => Vec::new(),
                _ => vec![u8::try_from(kind as u16).unwrap()],
            };
            PlantCompiledSection::new(kind, bytes)
        })
        .collect()
}
