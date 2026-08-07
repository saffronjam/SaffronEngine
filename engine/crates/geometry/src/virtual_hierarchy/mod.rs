//! Device-independent clustered geometry, aggregate voxels, and page hierarchy cooking.

mod codec;
mod cook;
mod support;
mod types;
mod validate;
mod voxel;

#[cfg(test)]
mod fixtures;

pub use codec::{
    DeformationSection, PageDirectorySection, RayTracingSection, TriangleHierarchySection,
    VoxelHierarchySection, decode_deformation, decode_page_directory,
    decode_portable_virtual_hierarchy, decode_portable_virtual_hierarchy_sections,
    decode_ray_tracing, decode_triangle_hierarchy, decode_voxel_hierarchy,
    encode_portable_virtual_hierarchy,
};
pub use cook::cook_portable_virtual_hierarchy;
pub use types::{
    AppearanceError, GeometryPrototype, HierarchyRepresentation, MicroInstance,
    PORTABLE_CLUSTER_MAX_TRIANGLES, PORTABLE_CLUSTER_MAX_VERTICES,
    PORTABLE_HIERARCHY_FORMAT_VERSION, PORTABLE_HIERARCHY_MAX_CHILDREN, PORTABLE_VOXEL_BRICK_EDGE,
    PortableAggregationMode, PortableBounds, PortableClusterVertex, PortableDeformationKind,
    PortableDeformationRegion, PortableHierarchyInput, PortableHierarchyNode,
    PortableHierarchyPage, PortableMaterialMoments, PortableOpacityMicromap,
    PortableRayTracingRecord, PortableSourceMesh, PortableSourceSkin, PortableSourceSubmesh,
    PortableSourceVertex, PortableTriangleCluster, PortableUseCombination,
    PortableVirtualHierarchy, PortableVoxelBrick, PortableVoxelVertex, VirtualHierarchyMaterial,
    VirtualMaterialClass, aggregate_virtual_hierarchy_materials,
};
pub use validate::{select_portable_hierarchy_cut, validate_portable_virtual_hierarchy};
