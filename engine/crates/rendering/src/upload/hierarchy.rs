use saffron_geometry::{Mesh, PortableVirtualHierarchy, validate_portable_virtual_hierarchy};

use crate::{Error, Result};

pub(super) fn validate_upload_hierarchy(
    mesh: &Mesh,
    hierarchy: &PortableVirtualHierarchy,
) -> Result<()> {
    validate_portable_virtual_hierarchy(hierarchy)
        .map_err(|error| Error::InvalidUploadData(error.to_string()))?;
    if hierarchy.prototypes.is_empty() {
        return Err(Error::InvalidUploadData(
            "portable hierarchy has no geometry prototype".to_owned(),
        ));
    }
    // The uploaded vertex stream is the prototypes' streams concatenated in prototype-id
    // order; the submesh table concatenates the per-prototype ranges the same way (a
    // single-prototype mesh with no authored submeshes uploads one implicit full range).
    let vertex_total: usize = hierarchy
        .prototypes
        .iter()
        .map(|prototype| prototype.vertex_count as usize)
        .sum();
    let submesh_total: usize = hierarchy
        .prototypes
        .iter()
        .map(|prototype| prototype.submesh_count as usize)
        .sum();
    if vertex_total != mesh.vertices.len() || submesh_total != mesh.submeshes.len().max(1) {
        return Err(Error::InvalidUploadData(
            "portable hierarchy does not describe the uploaded mesh".to_owned(),
        ));
    }
    Ok(())
}

/// The cooked micromaps for `hierarchy`, keyed by the flattened submesh each refines.
///
/// A row whose subdivision level exceeds the device cap is dropped rather than clamped: a block
/// decoded at the wrong level is not conservative, it is wrong. Dropping is safe — an absent
/// micromap only costs the classifier work it would have removed.
pub(super) fn cooked_micromap_builds(
    hierarchy: &PortableVirtualHierarchy,
    max_subdivision: u32,
) -> Vec<(u32, saffron_geometry::OpacityMicromapBuild)> {
    hierarchy
        .opacity_micromaps
        .iter()
        .filter(|micromap| {
            micromap
                .usage
                .iter()
                .all(|&(_, level, _)| level <= max_subdivision)
        })
        .map(|micromap| {
            (
                micromap.submesh,
                saffron_geometry::OpacityMicromapBuild {
                    indices: micromap.indices.clone(),
                    blocks: micromap
                        .blocks
                        .iter()
                        .map(|&(data_offset, subdivision_level, format)| {
                            saffron_geometry::MicromapTriangle {
                                data_offset,
                                subdivision_level,
                                format,
                            }
                        })
                        .collect(),
                    data: micromap.data.clone(),
                    usage: micromap
                        .usage
                        .iter()
                        .map(|&(count, subdivision_level, format)| {
                            saffron_geometry::MicromapUsage {
                                count,
                                subdivision_level,
                                format,
                            }
                        })
                        .collect(),
                    classes: saffron_geometry::MicromapClasses {
                        opaque: micromap.classes.0,
                        transparent: micromap.classes.1,
                        unknown: micromap.classes.2,
                    },
                },
            )
        })
        .collect()
}

/// Per-submesh opacity as the cooker classified it, in the mesh's global submesh order.
///
/// Each triangle cluster carries the `(prototype, source_submesh)` it came from and the material
/// class resolved at cook, so a BLAS build needs no runtime material resolution. Prototypes own
/// contiguous runs of the submesh table in id order.
///
/// A submesh no cluster covers — simplified away, or a degenerate range — is reported non-opaque,
/// the conservative direction: guessing opaque would commit hits on geometry nothing verified.
pub(super) fn cooked_submesh_opacity(
    hierarchy: &PortableVirtualHierarchy,
    submesh_count: usize,
) -> Vec<bool> {
    let mut by_pair = std::collections::BTreeMap::<(u32, u32), bool>::new();
    for cluster in &hierarchy.triangle_clusters {
        let opaque = cluster.material_class.is_opaque();
        by_pair
            .entry((cluster.prototype, cluster.source_submesh))
            // Clusters of one submesh share its material, so this only ever confirms. The `&&`
            // is what keeps a disagreement conservative rather than order-dependent.
            .and_modify(|resolved| *resolved = *resolved && opaque)
            .or_insert(opaque);
    }
    let mut opacity = Vec::with_capacity(submesh_count);
    for prototype in &hierarchy.prototypes {
        for source_submesh in 0..prototype.submesh_count {
            opacity.push(
                by_pair
                    .get(&(prototype.id, source_submesh))
                    .copied()
                    .unwrap_or(false),
            );
        }
    }
    opacity.resize(submesh_count, false);
    opacity
}

/// Builds the assembly table for a hierarchy that places prototypes through uses. `None` for the
/// trivial single-prototype, single-identity-use shape every plain mesh cooks to.
pub(super) fn assembly_from_hierarchy(
    hierarchy: &PortableVirtualHierarchy,
) -> Result<Option<crate::MeshAssembly>> {
    const IDENTITY_BITS: [i32; 16] = [
        65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
    ];
    let trivial = hierarchy.prototypes.len() == 1
        && hierarchy.micro_instances.len() == 1
        && hierarchy.micro_instances[0].prototype == 0
        && hierarchy.micro_instances[0].transform_bits == IDENTITY_BITS;
    if trivial {
        return Ok(None);
    }
    if hierarchy.micro_instances.is_empty() {
        return Err(Error::InvalidUploadData(
            "assembly hierarchy places no prototype uses".to_owned(),
        ));
    }
    // The parts table is indexed by prototype id, so the hierarchy's table must be
    // id-ordered (the cooker emits it that way).
    for (index, prototype) in hierarchy.prototypes.iter().enumerate() {
        if prototype.id as usize != index {
            return Err(Error::InvalidUploadData(
                "assembly hierarchy prototype table is not id-ordered".to_owned(),
            ));
        }
    }
    let mut prototypes = Vec::with_capacity(hierarchy.prototypes.len());
    let mut uses = Vec::with_capacity(hierarchy.micro_instances.len());
    // Each use carries its part's structural semantic tag so the wind branch modes
    // pick the response per part (trunk 0 … blade 7; absent parts read trunk).
    let semantic_by_part: std::collections::HashMap<u128, u32> = hierarchy
        .deformation
        .iter()
        .map(|region| (region.part, u32::from(region.semantic.0)))
        .collect();
    let mut vertex_base = 0_u64;
    for prototype in &hierarchy.prototypes {
        let first_use = u32::try_from(uses.len())
            .map_err(|_| Error::InvalidUploadData("assembly use table overflow".to_owned()))?;
        for instance in hierarchy
            .micro_instances
            .iter()
            .filter(|instance| instance.prototype == prototype.id)
        {
            let mut transform = [0.0_f32; 12];
            for (slot, bits) in transform
                .iter_mut()
                .zip(instance.transform_bits.iter().take(12))
            {
                *slot = *bits as f32 / 65_536.0;
            }
            uses.push(crate::GpuAssemblyUseRecord {
                transform,
                prototype: prototype.id,
                reserved: [
                    semantic_by_part.get(&instance.part).copied().unwrap_or(0),
                    0,
                    0,
                ],
            });
        }
        let use_count = u32::try_from(uses.len())
            .map_err(|_| Error::InvalidUploadData("assembly use table overflow".to_owned()))?
            - first_use;
        if use_count == 0 {
            return Err(Error::InvalidUploadData(
                "assembly hierarchy leaves a prototype unused".to_owned(),
            ));
        }
        // The executor's vertex base counts vertices (its fetch multiplies by the stride).
        let base = u32::try_from(vertex_base).map_err(|_| {
            Error::InvalidUploadData("assembly vertex stream exceeds u32 bases".to_owned())
        })?;
        prototypes.push(crate::GpuAssemblyPrototypeRecord {
            first_use,
            use_count,
            vertex_base: base,
            reserved: 0,
        });
        vertex_base = vertex_base
            .checked_add(u64::from(prototype.vertex_count))
            .ok_or_else(|| {
                Error::InvalidUploadData("assembly vertex stream overflow".to_owned())
            })?;
    }
    // The mask table: authored combinations, or one implicit all-active combination
    // for an assembly cooked without variation data.
    let mask_words = uses.len().div_ceil(32);
    let (combinations, masks) = if hierarchy.combinations.is_empty() {
        let mut words = vec![0_u32; mask_words];
        for use_index in 0..uses.len() {
            words[use_index / 32] |= 1 << (use_index % 32);
        }
        (vec![(0, 0)], words)
    } else {
        let mut identities = Vec::with_capacity(hierarchy.combinations.len());
        let mut words = Vec::with_capacity(hierarchy.combinations.len() * mask_words);
        for combination in &hierarchy.combinations {
            if combination.active_words.len() != mask_words {
                return Err(Error::InvalidUploadData(
                    "assembly combination mask does not span the use table".to_owned(),
                ));
            }
            identities.push((combination.variation, combination.phenotype));
            words.extend_from_slice(&combination.active_words);
        }
        (identities, words)
    };
    Ok(Some(crate::MeshAssembly {
        prototypes,
        uses,
        combinations,
        masks,
        // Filled by `upload_mesh`, where the flattened submesh table is in scope.
        prototype_slices: Vec::new(),
    }))
}

#[cfg(test)]
pub(crate) fn hierarchy_for_upload(
    mesh: &Mesh,
    skin: &[saffron_geometry::VertexSkin],
) -> Result<PortableVirtualHierarchy> {
    let input = saffron_geometry::PortableHierarchyInput::from_mesh(mesh, skin)
        .map_err(|error| Error::InvalidUploadData(error.to_string()))?;
    saffron_geometry::cook_portable_virtual_hierarchy(&input)
        .map_err(|error| Error::InvalidUploadData(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upload::fixtures::triangle;

    /// A plain mesh's cooked hierarchy (one prototype, one identity use) builds no
    /// assembly table; a multi-prototype hierarchy builds the id-ordered prototype
    /// records with prefix-summed vertex bases and prototype-grouped f32 use
    /// transforms.
    #[test]
    fn assembly_table_builds_for_multi_prototype_hierarchies_only() {
        let mesh = triangle();
        let hierarchy = hierarchy_for_upload(&mesh, &[]).expect("cook hierarchy");
        assert!(
            assembly_from_hierarchy(&hierarchy)
                .expect("trivial shape")
                .is_none(),
            "a plain mesh keeps its parts range empty"
        );

        const IDENTITY: [i32; 16] = [
            65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
        ];
        let mut translated = IDENTITY;
        translated[7] = 2 * 65_536; // row 1, column 3: +2 m along Y.
        let mut family = hierarchy.clone();
        let base = family.prototypes[0].clone();
        family.prototypes = vec![
            saffron_geometry::GeometryPrototype {
                id: 0,
                vertex_count: 3,
                ..base.clone()
            },
            saffron_geometry::GeometryPrototype {
                id: 1,
                vertex_count: 5,
                ..base
            },
        ];
        family.micro_instances = vec![
            saffron_geometry::MicroInstance {
                part: 1,
                prototype: 0,
                transform_bits: IDENTITY,
            },
            saffron_geometry::MicroInstance {
                part: 2,
                prototype: 1,
                transform_bits: translated,
            },
            saffron_geometry::MicroInstance {
                part: 3,
                prototype: 1,
                transform_bits: IDENTITY,
            },
        ];
        let assembly = assembly_from_hierarchy(&family)
            .expect("family shape")
            .expect("assembly table");
        assert_eq!(assembly.prototypes.len(), 2);
        assert_eq!(
            assembly.prototypes[0],
            crate::GpuAssemblyPrototypeRecord {
                first_use: 0,
                use_count: 1,
                vertex_base: 0,
                reserved: 0,
            }
        );
        assert_eq!(
            assembly.prototypes[1],
            crate::GpuAssemblyPrototypeRecord {
                first_use: 1,
                use_count: 2,
                vertex_base: 3,
                reserved: 0,
            }
        );
        assert_eq!(assembly.uses.len(), 3);
        assert_eq!(assembly.uses[1].prototype, 1);
        assert_eq!(
            assembly.uses[1].transform[7], 2.0,
            "row 1 translation in metres"
        );
        assert_eq!(assembly.uses[2].transform[0], 1.0, "identity scale");
        // No authored combinations → one implicit all-active mask word.
        assert_eq!(assembly.combinations, vec![(0, 0)]);
        assert_eq!(assembly.masks, vec![0b111]);
        assert_eq!(
            assembly.byte_len(),
            size_of::<crate::GpuAssemblyHeaderRecord>()
                + 2 * size_of::<crate::GpuAssemblyPrototypeRecord>()
                + 3 * size_of::<crate::GpuAssemblyUseRecord>()
                + size_of::<u32>()
        );
        assert_eq!(assembly.packed_bytes().len(), assembly.byte_len());
    }
}
