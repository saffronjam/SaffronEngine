//! Topology, limit, bounds, page-order, and root validation, plus the CPU reference cut.

use std::collections::BTreeSet;

use crate::Result;

use super::support::{bounds_contains, format_error};
use super::types::{
    HierarchyRepresentation, PORTABLE_CLUSTER_MAX_TRIANGLES, PORTABLE_CLUSTER_MAX_VERTICES,
    PortableVirtualHierarchy,
};

/// Validates hierarchy topology, compact limits, conservative bounds, page order, and roots.
pub fn validate_portable_virtual_hierarchy(hierarchy: &PortableVirtualHierarchy) -> Result<()> {
    if hierarchy.roots.is_empty() || hierarchy.pages.is_empty() {
        return Err(format_error("portable hierarchy", "roots"));
    }
    if hierarchy
        .triangle_clusters
        .iter()
        .enumerate()
        .any(|(id, cluster)| {
            let prototype = hierarchy.prototypes.get(cluster.prototype as usize);
            cluster.id as usize != id
                || prototype.is_none_or(|prototype| {
                    cluster.source_submesh >= prototype.submesh_count
                        || cluster
                            .source_vertices
                            .iter()
                            .any(|vertex| *vertex >= prototype.vertex_count)
                })
                || cluster.vertices.is_empty()
                || cluster.vertices.len() > PORTABLE_CLUSTER_MAX_VERTICES
                || cluster.source_vertices.len() != cluster.vertices.len()
                || cluster.local_indices.is_empty()
                || cluster.local_indices.len() / 3 > PORTABLE_CLUSTER_MAX_TRIANGLES
                || !cluster.local_indices.len().is_multiple_of(3)
                || cluster
                    .local_indices
                    .iter()
                    .any(|index| *index as usize >= cluster.vertices.len())
                || !bounds_contains(cluster.deformed_bounds, cluster.bounds)
                || cluster.page as usize >= hierarchy.pages.len()
        })
    {
        return Err(format_error("portable hierarchy", "triangleClusters"));
    }
    for (id, brick) in hierarchy.voxel_bricks.iter().enumerate() {
        let voxel_count = brick
            .dimensions
            .into_iter()
            .try_fold(1_usize, |total, value| {
                total.checked_mul(usize::from(value))
            });
        if brick.id as usize != id
            || voxel_count.is_none_or(|count| brick.occupancy.len() != count.div_ceil(8))
            || brick.indices.is_empty()
            || !brick.indices.len().is_multiple_of(3)
            || brick
                .indices
                .iter()
                .any(|index| *index as usize >= brick.vertices.len())
            || !bounds_contains(brick.deformed_bounds, brick.bounds)
            || brick.page as usize >= hierarchy.pages.len()
        {
            return Err(format_error("portable hierarchy", "voxelBricks"));
        }
    }
    for (id, node) in hierarchy.nodes.iter().enumerate() {
        if node.id as usize != id
            || node.page as usize >= hierarchy.pages.len()
            || !bounds_contains(node.deformed_bounds, node.bounds)
            || node.children.windows(2).any(|pair| pair[0] >= pair[1])
            // The traversal culls whole subtrees on one node's bounds; a child reaching
            // outside its parent would let a view reject geometry it can see.
            || node.children.iter().any(|child| {
                hierarchy.nodes.get(*child as usize).is_none_or(|child| {
                    !bounds_contains(node.bounds, child.bounds)
                        || !bounds_contains(node.deformed_bounds, child.deformed_bounds)
                })
            })
        {
            return Err(format_error("portable hierarchy", "nodes"));
        }
        match node.representation {
            HierarchyRepresentation::Triangles { first, count } => {
                if count == 0
                    || first
                        .checked_add(count)
                        .is_none_or(|end| end as usize > hierarchy.triangle_clusters.len())
                {
                    return Err(format_error("portable hierarchy", "node.triangles"));
                }
            }
            HierarchyRepresentation::Voxel { brick }
                if brick as usize >= hierarchy.voxel_bricks.len() =>
            {
                return Err(format_error("portable hierarchy", "node.voxel"));
            }
            HierarchyRepresentation::Voxel { .. } => {}
        }
        for child in &node.children {
            let child_node = hierarchy
                .nodes
                .get(*child as usize)
                .ok_or_else(|| format_error("portable hierarchy", "node.child"))?;
            if child_node.parent != Some(node.id)
                || child_node.appearance_error.total > node.appearance_error.total
            {
                return Err(format_error("portable hierarchy", "node.refinement"));
            }
        }
    }
    for (id, page) in hierarchy.pages.iter().enumerate() {
        if page.id as usize != id
            || page.node as usize >= hierarchy.nodes.len()
            || hierarchy.nodes[page.node as usize].page != page.id
            || page.dependency.is_some_and(|parent| parent >= page.id)
            || page.guaranteed_root != page.dependency.is_none()
        {
            return Err(format_error("portable hierarchy", "pages"));
        }
    }
    for root in &hierarchy.roots {
        let node = hierarchy
            .nodes
            .get(*root as usize)
            .ok_or_else(|| format_error("portable hierarchy", "root.node"))?;
        let page = &hierarchy.pages[node.page as usize];
        if node.parent.is_some()
            || !page.guaranteed_root
            || !matches!(node.representation, HierarchyRepresentation::Voxel { .. })
        {
            return Err(format_error("portable hierarchy", "root.drawable"));
        }
    }
    Ok(())
}

/// Selects a hole-free CPU reference cut from complete-child residency and appearance error.
pub fn select_portable_hierarchy_cut(
    hierarchy: &PortableVirtualHierarchy,
    resident_pages: &BTreeSet<u32>,
    maximum_error: u32,
) -> Result<Vec<u32>> {
    validate_portable_virtual_hierarchy(hierarchy)?;
    let mut cut = Vec::new();
    for root in &hierarchy.roots {
        select_node_cut(hierarchy, *root, resident_pages, maximum_error, &mut cut)?;
    }
    cut.sort_unstable();
    Ok(cut)
}

fn select_node_cut(
    hierarchy: &PortableVirtualHierarchy,
    node_id: u32,
    resident_pages: &BTreeSet<u32>,
    maximum_error: u32,
    cut: &mut Vec<u32>,
) -> Result<()> {
    let node = &hierarchy.nodes[node_id as usize];
    if !resident_pages.contains(&node.page) {
        return Err(format_error("portable hierarchy cut", "rootResidency"));
    }
    let refine = node.appearance_error.total > maximum_error
        && !node.children.is_empty()
        && node
            .children
            .iter()
            .all(|child| resident_pages.contains(&hierarchy.nodes[*child as usize].page));
    if refine {
        for child in &node.children {
            select_node_cut(hierarchy, *child, resident_pages, maximum_error, cut)?;
        }
    } else {
        cut.push(node_id);
    }
    Ok(())
}
