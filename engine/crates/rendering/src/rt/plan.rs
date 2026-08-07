//! Assembling this frame's acceleration-structure work: the TLAS instance packing, the
//! skinned and tessellated BLAS plans, the partitioned-TLAS op stream, and the capacity
//! growth the plans need.

use super::*;

impl Rt {
    /// Prepares the per-frame TLAS build: refit-plans each skinned BLAS (creating the AS on
    /// first sight, sizing the shared scratch), packs the instance buffer, (re)creates the
    /// TLAS + scratch on a capacity change, and writes the TLAS into set 6 — every step that
    /// touches `&mut self`. Returns an owned, `'static` [`TlasBuildPlan`] of device-address
    /// build descriptors the `tlas-build` pass replays via [`record_tlas_build_plan`]; the
    /// plan holds the `Arc<AccelerationStructure>`s so they outlive the recording. The prep
    /// and record halves are split to fit the `'static` graph closure.
    ///
    /// Returns `None` (and leaves `tlas_ready` false) when RT is unsupported, no instances
    /// exist, or a build resource cannot be created.
    pub fn prepare_tlas_build(
        &mut self,
        device: &Device,
        frame: usize,
        deformed: &[DeformedRtInstance],
        deformed_buffer: Option<vk::Buffer>,
        micro: &[crate::MicroRtTile],
        cut_view: RtCutView,
    ) -> Option<TlasBuildPlan> {
        self.tlas_ready = false;
        self.skinned_blas_count = 0;
        self.tessellated_blas_count = 0;
        if !self.supported
            || (self.scene.instances.is_empty() && deformed.is_empty() && micro.is_empty())
        {
            return None;
        }
        let dispatch = self.dispatch.clone()?;

        // Every structure over minted topology — an amplified instance's dice output and a
        // materialized field tile's blades alike — takes the same full-`BUILD` path, because
        // variable topology every frame forbids the in-place `UPDATE` a refit uses.
        let generated: Vec<GeneratedRtGeometry> = deformed
            .iter()
            .filter_map(|inst| {
                inst.tess.map(|slice| GeneratedRtGeometry {
                    key: inst.entity,
                    slice,
                    world_transform: inst.world_transform,
                })
            })
            .chain(micro.iter().map(|tile| GeneratedRtGeometry {
                key: tile.key,
                slice: tile.slice,
                // The materialized blades are world-space, as compute skinning's are.
                world_transform: Mat4::IDENTITY,
            }))
            .collect();

        // Plan each deforming BLAS: skinned/morph refit (create-once then in-place `UPDATE`) +
        // the generated-topology full `BUILD`s, both sizing the shared build scratch.
        let mut blas_ops =
            self.plan_skinned_blas_refits(device, &dispatch, frame, deformed, deformed_buffer);
        let skinned_op_count = blas_ops.len() as u32;
        let (tess_ops, generated_placed) =
            self.plan_generated_blas_builds(device, &dispatch, frame, &generated);
        let tess_op_count = tess_ops.len() as u32;
        blas_ops.extend(tess_ops);

        // One placement per static mesh that has a BLAS, then one per deforming instance.
        // Placements are the single description both top-level forms derive from: the KHR
        // path packs them into its instance array, the partitioned path diffs them against
        // the structure's placed state. Deriving each independently would let the two
        // top-levels disagree about the same scene.
        let mut placements: Vec<Placement> =
            Vec::with_capacity(self.scene.instances.len() + deformed.len());
        let mut aggregate_count = 0_u32;
        for (scene_index, input) in self.scene.instances.iter().enumerate() {
            let primary = placement_primary(input.instance_slot, scene_index);
            // Representation selection, by the raster traversal's own refine test: project
            // the root cut's appearance error through the instance scale and eye distance,
            // and when it fits under the threshold place the single family-space aggregate
            // structure instead of the fine expansion — the same swap the traversal makes
            // when it draws the root's voxel bricks.
            if let Some(blas) = aggregate_stands_in(input, &cut_view) {
                placements.push(Placement {
                    key: crate::rt_ptlas::PtlasKey { primary, sub: 0 },
                    rows: transform_rows(&input.model),
                    // The aggregate merges the family's submeshes into voxel-brick surfaces, so no
                    // submesh names its geometry and no candidate resolves against one. Forced
                    // opaque below, which is what keeps the classifier out of this representation.
                    instance_slot: RT_UNMIRRORED_INSTANCE,
                    first_submesh: 0,
                    // The aggregate is built opaque; an opacity override targets the fine
                    // geometry's coverage classes, which this representation merged away.
                    opacity: vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE,
                    blas: crate::RtBlas::Khr(blas),
                });
                aggregate_count += 1;
                continue;
            }
            // An assembly is not one structure: KHR acceleration structures cannot nest
            // micro-instance parts, so a family expands into one TLAS instance per active
            // placed use, each referencing its prototype's structure.
            if let Some(assembly) = input.mesh.assembly.as_ref() {
                if input.mesh.assembly_blas.len() != assembly.prototypes.len() {
                    continue;
                }
                let words = assembly.mask_words();
                let base = input.combination as usize * words;
                for (use_index, use_record) in assembly.uses.iter().enumerate() {
                    let word = assembly.masks.get(base + use_index / 32).copied();
                    if word.is_none_or(|bits| bits & (1 << (use_index % 32)) == 0) {
                        continue;
                    }
                    let Some(blas) = input.mesh.assembly_blas.get(use_record.prototype as usize)
                    else {
                        continue;
                    };
                    let Some(slice) = assembly.prototype_slices.get(use_record.prototype as usize)
                    else {
                        continue;
                    };
                    placements.push(Placement {
                        // The use index distinguishes a family's placements under one scene
                        // slot; +1 keeps it clear of the single-placement zero.
                        key: crate::rt_ptlas::PtlasKey {
                            primary,
                            sub: use_index as u32 + 1,
                        },
                        rows: transform_rows(&(input.model * assembly_use_matrix(use_record))),
                        instance_slot: input.instance_slot,
                        first_submesh: slice.first_submesh,
                        opacity: instance_opacity_flags(input.opacity_override),
                        blas: blas.clone(),
                    });
                }
                continue;
            }
            let Some(blas) = input.mesh.blas.as_ref() else {
                continue;
            };
            placements.push(Placement {
                key: crate::rt_ptlas::PtlasKey { primary, sub: 0 },
                rows: transform_rows(&input.model),
                instance_slot: input.instance_slot,
                first_submesh: 0,
                opacity: instance_opacity_flags(input.opacity_override),
                blas: crate::RtBlas::Khr(Arc::clone(blas)),
            });
        }
        // A refit instance references its BLAS at its `world_transform`: identity for a skinned
        // (or skin+morph) instance — the deformed vertices are already in world space — and the
        // node world matrix for an unskinned-morph instance, whose vertices are mesh-local. Its
        // structure carries the same per-submesh opacity classes and submesh span its static
        // sibling does, so a masked leaf card surfaces candidates for the coverage classifier here
        // too rather than committing as a solid quad.
        for inst in deformed {
            if inst.tess.is_some() {
                continue;
            }
            let Some(accel) = self.frames[frame]
                .skinned_blas
                .get(&inst.entity)
                .map(|slot| Arc::clone(&slot.accel))
            else {
                continue;
            };
            placements.push(Placement {
                key: crate::rt_ptlas::PtlasKey {
                    primary: DEFORMED_PRIMARY_BASE | inst.entity,
                    sub: 0,
                },
                rows: transform_rows(&inst.world_transform),
                instance_slot: inst.instance_slot,
                first_submesh: inst.first_submesh,
                opacity: instance_opacity_flags(inst.opacity_override),
                blas: crate::RtBlas::Khr(accel),
            });
        }
        // Generated topology names no submesh — amplification merges the base submeshes into one
        // stream and a materialized field tile mints its blades outright — so no candidate on one
        // resolves against a scene record. Both forms are real closed geometry rather than a
        // coverage-masked card (a blade is a tapered strip, a diced patch is displaced surface), so
        // forcing them opaque commits exactly the hits their triangles already describe.
        for planned in &generated_placed {
            placements.push(Placement {
                key: crate::rt_ptlas::PtlasKey {
                    primary: DEFORMED_PRIMARY_BASE | planned.key,
                    sub: 0,
                },
                rows: transform_rows(&planned.world_transform),
                instance_slot: RT_UNMIRRORED_INSTANCE,
                first_submesh: 0,
                opacity: vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE,
                blas: crate::RtBlas::Khr(Arc::clone(&planned.accel)),
            });
        }

        let count = placements.len() as u32;
        if count == 0 {
            return None;
        }
        let retained: Vec<crate::RtBlas> = placements
            .iter()
            .map(|placement| placement.blas.clone())
            .collect();
        // The partitioned structure is the whole top level where it exists: it consumes the
        // placements directly and retains what it places, so none of the KHR instance array,
        // its buffer, or its per-frame TLAS is reached on such a device.
        if self.ptlas.is_some() {
            return self.plan_ptlas_build(
                device,
                frame,
                &placements,
                &retained,
                PlanContext {
                    dispatch,
                    blas_ops,
                    count,
                    aggregate_count,
                    skinned_op_count,
                    tess_op_count,
                },
            );
        }
        // `instanceCustomIndex` is the placement's own position, which the ray-instance table
        // resolves back to a scene record. Packing the scene slot directly cannot work: a slot does
        // not say which of a family's prototype structures the candidate hit, and the field is 24
        // bits where a slot is 32.
        let instances: Vec<vk::AccelerationStructureInstanceKHR> = placements
            .iter()
            .enumerate()
            .map(|(index, placement)| {
                make_instance(
                    placement.rows,
                    index as u32,
                    placement.opacity,
                    placement.blas.address(),
                )
            })
            .collect();
        let mut retained = retained;
        if let Err(err) = self.ensure_tlas_capacity(frame, count) {
            tracing::error!("rt: TLAS instance buffer grow failed: {err}");
            return None;
        }
        self.write_ray_instances(frame, &placements);
        // Copy the packed instances into the host-visible instance buffer. The ash
        // `AccelerationStructureInstanceKHR` is not `bytemuck::Pod` (it embeds bit-packed
        // unions), so view it as raw bytes for the memcpy.
        {
            // SAFETY: `instances` is a contiguous, fully-initialized `#[repr(C)]` array; the
            // byte view spans exactly its bytes and is only read into the mapped buffer.
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    instances.as_ptr().cast::<u8>(),
                    std::mem::size_of_val(instances.as_slice()),
                )
            };
            let buffer = self.frames[frame]
                .instance_buffer
                .as_mut()
                .expect("instance buffer present after ensure_tlas_capacity");
            if let Some(dst) = buffer.mapped_bytes() {
                dst[..bytes.len()].copy_from_slice(bytes);
            }
        }

        // Size + (re)create the TLAS on a capacity change, then write it into set 6.
        let tlas_op = self.prepare_tlas(device, &dispatch, frame, count)?;
        let blas_scratch_addr = self.frames[frame]
            .blas_scratch
            .as_ref()
            .map(|b| device.buffer_device_address(b.handle()))
            .unwrap_or(0);

        self.frame_instance_count = count;
        self.blas_count = distinct_blas_count(&retained);
        self.skinned_blas_count = skinned_op_count;
        self.tessellated_blas_count = tess_op_count;
        self.aggregate_instance_count = aggregate_count;
        self.resolvable_instance_count = resolvable_placements(&placements);
        // Sum the bottom-level storage before the TLAS joins `retained`, so the two tiers stay
        // separable in the telemetry. Structures are deduplicated by device address for the same
        // reason `distinct_blas_count` is: instances of one mesh share a structure, and counting
        // its bytes once per instance would report sharing as growth.
        let (blas_bytes, blas_built_bytes) = distinct_blas_bytes(&retained);
        self.blas_bytes = blas_bytes;
        self.blas_built_bytes = blas_built_bytes;
        let (cluster_blas_count, clas_count) = distinct_cluster_blas(&retained);
        self.cluster_blas_count = cluster_blas_count;
        self.clas_count = clas_count;
        let (omm_micromaps, omm_classes) = distinct_micromap_classes(&self.scene.instances);
        self.omm_micromaps = omm_micromaps;
        self.omm_classes = omm_classes;
        self.tlas_bytes = self.frames[frame]
            .tlas
            .as_ref()
            .map_or(0, |tlas| tlas.size());
        self.scratch_bytes = self.frames[frame]
            .scratch
            .as_ref()
            .map_or(0, |scratch| scratch.size())
            + self.frames[frame]
                .blas_scratch
                .as_ref()
                .map_or(0, |scratch| scratch.size());
        self.tlas_ready = true;
        // Retain the TLAS too (it is referenced only through `self` otherwise, but holding
        // it in the plan keeps the replay self-contained).
        retained.push(crate::RtBlas::Khr(Arc::clone(
            self.frames[frame].tlas.as_ref().expect("TLAS present"),
        )));
        Some(TlasBuildPlan {
            dispatch,
            blas_ops,
            blas_scratch_addr,
            top: TopLevelBuild::Khr(tlas_op),
            _retained: retained,
        })
    }

    /// Plans each deforming instance's BLAS refit: creates the AS on first sight, sizes the
    /// shared scratch, and records the build mode (`BUILD` first, then in-place `UPDATE`).
    /// The first-sight build reads the live deformed buffer — which the morph + skin passes
    /// already wrote this frame — so it builds over the resolved-weight pose, never the
    /// zero-weight base. The recording is deferred to [`record_tlas_build_plan`].
    pub(super) fn plan_skinned_blas_refits(
        &mut self,
        device: &Device,
        dispatch: &accel::Device,
        frame: usize,
        instances: &[DeformedRtInstance],
        deformed_buffer: Option<vk::Buffer>,
    ) -> Vec<BlasRefitOp> {
        let Some(deformed) = deformed_buffer else {
            return Vec::new();
        };
        if instances.is_empty() {
            return Vec::new();
        }
        let deformed_base = device.buffer_device_address(deformed);
        let vertex_stride = size_of::<Vertex>() as vk::DeviceSize;
        // Retire the materialized-wind structures this frame did not ask for. A skinned entity's
        // structure is bounded by the entities that exist, but a materialized use's key names a
        // scene slot, and a camera crossing a vegetated world would otherwise accumulate one per
        // plant it ever passed. The frame slot's fence was waited before it was reused, so nothing
        // in flight still traces these.
        let wanted: std::collections::HashSet<u64> = instances
            .iter()
            .filter(|inst| inst.entity & WIND_BLAS_KEY_BASE != 0)
            .map(|inst| inst.entity)
            .collect();
        self.frames[frame]
            .skinned_blas
            .retain(|key, _| *key & WIND_BLAS_KEY_BASE == 0 || wanted.contains(key));

        let mut ops: Vec<BlasRefitOp> = Vec::with_capacity(instances.len());
        let mut scratch_needed: vk::DeviceSize = 0;
        for inst in instances {
            // A generated-topology instance takes the full-BUILD path
            // (`plan_generated_blas_builds`): variable topology every frame forbids the in-place
            // `UPDATE` this refit path uses.
            if skinned_refit_skips(
                inst.vertex_count,
                inst.index_count,
                inst.entity,
                inst.tess.is_some(),
            ) {
                continue;
            }
            let geometries = refit_geometries(inst);
            if geometries.is_empty() {
                continue;
            }
            // The slice mirrors the mesh's vertices from `vertex_base` on while the index stream
            // addresses them absolutely, so the build's vertex base is rebased by that much. The
            // allocator never places a slice below its own base, which is what keeps this address
            // inside the buffer.
            let vertex_data = deformed_base
                + vk::DeviceAddress::from(inst.deformed_offset - inst.vertex_base) * vertex_stride;
            let max_vertex = inst.vertex_base + inst.vertex_count - 1;
            let index_data = device.buffer_device_address(inst.mesh.index_buffer());

            let inputs: Vec<GeometryInputs<'_>> = geometries
                .iter()
                .map(|geometry| {
                    GeometryInputs::new(
                        vertex_data,
                        vertex_stride,
                        max_vertex + 1,
                        index_data,
                        geometry.opaque,
                        None,
                    )
                })
                .collect();
            let geoms: Vec<vk::AccelerationStructureGeometryKHR<'_>> =
                inputs.iter().map(GeometryInputs::geometry).collect();
            let triangle_counts: Vec<u32> = geometries
                .iter()
                .map(|geometry| geometry.triangle_count)
                .collect();
            let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
                .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
                .flags(
                    vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
                        | vk::BuildAccelerationStructureFlagsKHR::ALLOW_UPDATE,
                )
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(&geoms);
            let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
            // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()`.
            unsafe {
                dispatch.get_acceleration_structure_build_sizes(
                    vk::AccelerationStructureBuildTypeKHR::DEVICE,
                    &size_info,
                    &triangle_counts,
                    &mut sizes,
                );
            }

            // Build the AS on first sight; refit (in-place `UPDATE`) afterwards.
            let (accel, update) = match self.frames[frame].skinned_blas.get(&inst.entity) {
                Some(slot) => (Arc::clone(&slot.accel), slot.built),
                None => {
                    match AccelerationStructure::create(
                        &self.resources,
                        dispatch,
                        sizes.acceleration_structure_size,
                        vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
                    ) {
                        Ok(accel) => {
                            let accel = Arc::new(accel);
                            self.frames[frame].skinned_blas.insert(
                                inst.entity,
                                SkinnedBlas {
                                    accel: Arc::clone(&accel),
                                    built: false,
                                },
                            );
                            (accel, false)
                        }
                        Err(err) => {
                            tracing::error!("rt: skinned BLAS create failed: {err}");
                            continue;
                        }
                    }
                }
            };
            let want = if update {
                sizes.update_scratch_size
            } else {
                sizes.build_scratch_size
            };
            scratch_needed = scratch_needed.max(want);
            // Each refit is now considered built (the recording will run this frame).
            if let Some(slot) = self.frames[frame].skinned_blas.get_mut(&inst.entity) {
                slot.built = true;
            }
            ops.push(BlasRefitOp {
                dst: accel.handle(),
                vertex_data,
                vertex_stride,
                max_vertex,
                index_data,
                geometries,
                update,
            });
        }
        if ops.is_empty() {
            return Vec::new();
        }
        if let Err(err) = self.ensure_blas_scratch(frame, scratch_needed) {
            tracing::error!("rt: skinned BLAS scratch grow failed: {err}");
            // Roll back the "built" flags so a later frame retries the build cleanly.
            return Vec::new();
        }
        ops
    }

    /// Plans a full `MODE_BUILD` for each generated-topology structure over its slice of a
    /// transient arena — an amplified instance's dice output and a materialized micro-field tile's
    /// blades alike. Unlike the skinned refit there is no create-once/`UPDATE` gate: variable
    /// topology every frame demands a full rebuild, so the AS is sized to the worst-case primitive
    /// count and recreated only when that bound changes (never on the per-frame GPU-packed count).
    /// The build range runs the worst-case count too — the producing kernel degenerate-pads the
    /// index tail, so the extra triangles collapse to points the builder discards, giving a
    /// watertight, portable floor with no GPU-count readback. Shares the frame's build scratch
    /// (grown to the max, serialized by the recorder).
    ///
    /// Returns the build ops beside the structures they target, one entry each, so the top level
    /// places exactly the set this frame rebuilds.
    pub(super) fn plan_generated_blas_builds(
        &mut self,
        device: &Device,
        dispatch: &accel::Device,
        frame: usize,
        geometries: &[GeneratedRtGeometry],
    ) -> (Vec<BlasRefitOp>, Vec<PlannedGeneratedBlas>) {
        let vertex_stride = size_of::<Vertex>() as vk::DeviceSize;
        let mut ops: Vec<BlasRefitOp> = Vec::new();
        let mut placed: Vec<PlannedGeneratedBlas> = Vec::new();
        let mut scratch_needed: vk::DeviceSize = 0;
        for geometry in geometries {
            let tess = geometry.slice;
            if geometry.key == 0 || tess.worst_case_prims == 0 || tess.worst_case_verts == 0 {
                continue;
            }
            let vertex_data = device.buffer_device_address(tess.vertex_buffer)
                + vk::DeviceAddress::from(tess.vertex_base) * vertex_stride;
            let index_data = device.buffer_device_address(tess.index_buffer)
                + vk::DeviceAddress::from(tess.index_base) * size_of::<u32>() as vk::DeviceSize;

            let inputs = GeometryInputs::new(
                vertex_data,
                vertex_stride,
                tess.worst_case_verts,
                index_data,
                true,
                None,
            );
            let geom = inputs.geometry();
            let geoms = [geom];
            let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
                .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
                .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(&geoms);
            let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
            // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()` (1).
            unsafe {
                dispatch.get_acceleration_structure_build_sizes(
                    vk::AccelerationStructureBuildTypeKHR::DEVICE,
                    &size_info,
                    &[tess.worst_case_prims],
                    &mut sizes,
                );
            }

            // Reuse the AS while its worst-case bound holds; recreate it (never `UPDATE`) otherwise.
            let accel = match self.frames[frame].tessellated_blas.get(&geometry.key) {
                Some(slot)
                    if tess_blas_reuse(Some(slot.worst_case_prims), tess.worst_case_prims) =>
                {
                    Arc::clone(&slot.accel)
                }
                _ => match AccelerationStructure::create(
                    &self.resources,
                    dispatch,
                    sizes.acceleration_structure_size,
                    vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
                ) {
                    Ok(accel) => {
                        let accel = Arc::new(accel);
                        self.frames[frame].tessellated_blas.insert(
                            geometry.key,
                            TessellatedBlas {
                                accel: Arc::clone(&accel),
                                worst_case_prims: tess.worst_case_prims,
                            },
                        );
                        accel
                    }
                    Err(err) => {
                        tracing::error!("rt: generated-geometry BLAS create failed: {err}");
                        continue;
                    }
                },
            };
            scratch_needed = scratch_needed.max(sizes.build_scratch_size);
            placed.push(PlannedGeneratedBlas {
                key: geometry.key,
                world_transform: geometry.world_transform,
                accel: Arc::clone(&accel),
            });
            ops.push(BlasRefitOp {
                dst: accel.handle(),
                vertex_data,
                vertex_stride,
                max_vertex: tess.worst_case_verts - 1,
                index_data,
                // One geometry: the producer mints its own topology, so no submesh slice
                // describes the stream it emits.
                geometries: vec![RefitGeometry {
                    first_index: 0,
                    triangle_count: tess.worst_case_prims,
                    opaque: true,
                }],
                update: false,
            });
        }
        if ops.is_empty() {
            return (Vec::new(), Vec::new());
        }
        if let Err(err) = self.ensure_blas_scratch(frame, scratch_needed) {
            tracing::error!("rt: generated-geometry BLAS scratch grow failed: {err}");
            return (Vec::new(), Vec::new());
        }
        (ops, placed)
    }

    /// Plans the partitioned structure's advance: assigns each placement its stable slot,
    /// diffs against what the structure already holds, and points set 6 at the result.
    ///
    /// The whole top level, so it reports the same counters the KHR path does — with the
    /// partition and op counts beside them, which is where the difference shows.
    pub(super) fn plan_ptlas_build(
        &mut self,
        device: &Device,
        frame: usize,
        placements: &[Placement],
        retained: &[crate::RtBlas],
        context: PlanContext,
    ) -> Option<TlasBuildPlan> {
        let instances: Vec<crate::rt_ptlas::PtlasInstance> = placements
            .iter()
            .map(|placement| crate::rt_ptlas::PtlasInstance {
                key: placement.key,
                transform: placement.rows,
                mask: 0xFF,
                flags: crate::rt_ptlas::instance_flags(
                    vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE | placement.opacity,
                ),
                blas: placement.blas.clone(),
                // A deforming instance has no fixed cell — its structure is refit every
                // frame anyway — so it goes global rather than churning partitions.
                partition: if placement.key.primary & DEFORMED_PRIMARY_BASE != 0 {
                    crate::rt_ptlas::GLOBAL_PARTITION
                } else {
                    crate::rt_ptlas::partition_for_translation([
                        placement.rows[3],
                        placement.rows[7],
                        placement.rows[11],
                    ])
                },
            })
            .collect();
        let resources = Arc::clone(&self.resources);
        let (op, slots) = self.ptlas.as_mut()?.plan(&resources, frame, &instances)?;
        self.write_ray_instances_at(frame, placements, &slots);
        let ptlas = self.ptlas.as_ref()?;
        let address = ptlas.current_address()?;
        let structure_bytes = ptlas.structure_bytes();
        let ptlas_scratch_bytes = ptlas.scratch_bytes();
        self.write_mesh_set_ptlas(device, frame, address);

        let blas_scratch_addr = self.frames[frame]
            .blas_scratch
            .as_ref()
            .map(|b| device.buffer_device_address(b.handle()))
            .unwrap_or(0);
        self.frame_instance_count = context.count;
        self.skinned_blas_count = context.skinned_op_count;
        self.tessellated_blas_count = context.tess_op_count;
        self.aggregate_instance_count = context.aggregate_count;
        self.resolvable_instance_count = resolvable_placements(placements);
        self.blas_count = distinct_blas_count(retained);
        let (blas_bytes, blas_built_bytes) = distinct_blas_bytes(retained);
        self.blas_bytes = blas_bytes;
        self.blas_built_bytes = blas_built_bytes;
        let (cluster_blas_count, clas_count) = distinct_cluster_blas(retained);
        self.cluster_blas_count = cluster_blas_count;
        self.clas_count = clas_count;
        let (omm_micromaps, omm_classes) = distinct_micromap_classes(&self.scene.instances);
        self.omm_micromaps = omm_micromaps;
        self.omm_classes = omm_classes;
        self.tlas_bytes = structure_bytes;
        self.scratch_bytes = ptlas_scratch_bytes
            + self.frames[frame]
                .blas_scratch
                .as_ref()
                .map_or(0, |scratch| scratch.size());
        self.tlas_ready = true;
        Some(TlasBuildPlan {
            dispatch: context.dispatch,
            blas_ops: context.blas_ops,
            blas_scratch_addr,
            top: TopLevelBuild::Partitioned(op),
            // The structure retains what it places for as long as it places it, so this
            // holds only what the recording itself touches.
            _retained: retained.to_vec(),
        })
    }

    /// Writes the partitioned structure's device address into `frame`'s set 6.
    ///
    /// Unlike the KHR TLAS, which is rewritten only when its capacity changes, the
    /// partitioned structure alternates frame slots — each build writes the slot the other
    /// frame is not tracing — so the binding moves every frame.
    pub(super) fn write_mesh_set_ptlas(
        &self,
        device: &Device,
        frame: usize,
        address: vk::DeviceAddress,
    ) {
        let addresses = [address];
        let mut accel_write =
            crate::vk_nv_ptlas::WriteDescriptorSetPartitionedAccelerationStructureNV {
                acceleration_structure_count: 1,
                p_acceleration_structures: addresses.as_ptr(),
                ..Default::default()
            };
        let mut write = vk::WriteDescriptorSet::default()
            .dst_set(self.frames[frame].mesh_set)
            .dst_binding(0)
            .descriptor_type(
                crate::vk_nv_ptlas::DESCRIPTOR_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_NV,
            );
        write.descriptor_count = 1;
        // The transcribed payload cannot ride ash's typed `push_next`, so it is chained by
        // hand; nothing else is in this write's chain.
        write.p_next = (&raw mut accel_write).cast();
        // SAFETY: the ash seam. The set + layout are this renderer's; written on the render
        // thread after the slot's fence is waited (no concurrent host access), and both the
        // chained payload and the address array outlive the call.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Sizes + (re)creates the frame's TLAS on a capacity change, writing it into set 6, and
    /// returns the build op (handle + instance/scratch addresses + count).
    pub(super) fn prepare_tlas(
        &mut self,
        device: &Device,
        dispatch: &accel::Device,
        frame: usize,
        count: u32,
    ) -> Option<TlasBuildOp> {
        let instance_address = device.buffer_device_address(
            self.frames[frame]
                .instance_buffer
                .as_ref()
                .expect("instance buffer present")
                .handle(),
        );
        let geom = instances_geometry(instance_address);
        let geoms = [geom];
        let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
            .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
            .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
            .geometries(&geoms);

        // Size for the buffer capacity (>= count) so the TLAS is stable until the buffer
        // regrows; query both that and the actual count's scratch.
        let capacity = self.frames[frame].instance_capacity;
        let mut cap_sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()` (1).
        unsafe {
            dispatch.get_acceleration_structure_build_sizes(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &size_info,
                &[capacity],
                &mut cap_sizes,
            );
            dispatch.get_acceleration_structure_build_sizes(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &size_info,
                &[count],
                &mut sizes,
            );
        }

        if self.frames[frame].tlas_capacity < count {
            match AccelerationStructure::create(
                &self.resources,
                dispatch,
                cap_sizes.acceleration_structure_size,
                vk::AccelerationStructureTypeKHR::TOP_LEVEL,
            ) {
                Ok(tlas) => {
                    let tlas = Arc::new(tlas);
                    let handle = tlas.handle();
                    self.frames[frame].tlas = Some(tlas);
                    self.frames[frame].tlas_capacity = capacity;
                    self.write_mesh_set(device, frame, handle);
                }
                Err(err) => {
                    tracing::error!("rt: TLAS create failed: {err}");
                    return None;
                }
            }
        }
        let scratch_needed = sizes.build_scratch_size.max(cap_sizes.build_scratch_size);
        if let Err(err) = self.ensure_tlas_scratch(frame, scratch_needed) {
            tracing::error!("rt: TLAS scratch grow failed: {err}");
            return None;
        }
        let scratch_addr = device.buffer_device_address(
            self.frames[frame]
                .scratch
                .as_ref()
                .expect("TLAS scratch present after ensure")
                .handle(),
        );
        let dst = self.frames[frame]
            .tlas
            .as_ref()
            .expect("TLAS present after (re)create")
            .handle();
        Some(TlasBuildOp {
            dst,
            instance_address,
            scratch_address: scratch_addr,
            count,
        })
    }

    /// The most placements this frame's captured scene can expand into: an assembly contributes
    /// its whole use table (no combination selects more), everything else one, plus one per
    /// deforming instance and one per materialized micro-field tile. The bound the ray-instance
    /// table is sized from.
    ///
    /// The tile term is unconditional because the table is sized when the frame's address block is
    /// published, which is before the materialization decides how many tiles it reserves.
    ///
    /// A partitioned structure addresses the table by its own slot rather than by position, and a
    /// slot freed by a departed instance keeps its index, so the table also has to reach the whole
    /// slot table — which only ever grew because that many instances were live at once.
    pub(super) fn placement_upper_bound(&self, deformed_count: usize) -> u32 {
        let slots = self
            .ptlas
            .as_ref()
            .map_or(0, crate::rt_ptlas::Ptlas::slot_count);
        let statics: usize = self
            .scene
            .instances
            .iter()
            .map(|input| {
                input
                    .mesh
                    .assembly
                    .as_ref()
                    .map_or(1, |assembly| assembly.uses.len().max(1))
            })
            .sum();
        u32::try_from(
            statics
                .saturating_add(deformed_count)
                .saturating_add(crate::MICRO_RT_MAX_TILES as usize),
        )
        .unwrap_or(u32::MAX)
        .max(slots)
    }

    /// Ensures `frame`'s ray-instance identity table holds `count` records (host-visible + BDA),
    /// growing to the next power of two.
    pub(super) fn ensure_ray_instance_capacity(&mut self, frame: usize, count: u32) -> Result<()> {
        if self.frames[frame].ray_instances.is_some()
            && self.frames[frame].ray_instance_capacity >= count
        {
            return Ok(());
        }
        let mut capacity = self.frames[frame]
            .ray_instance_capacity
            .max(INITIAL_TLAS_CAPACITY);
        while capacity < count {
            capacity *= 2;
        }
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let buffer = Buffer::new(
            &self.resources,
            vk::DeviceSize::from(capacity) * size_of::<GpuRayInstanceRecord>() as vk::DeviceSize,
            vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &alloc_info,
        )?;
        self.frames[frame].ray_instances = Some(buffer);
        self.frames[frame].ray_instance_capacity = capacity;
        Ok(())
    }

    /// Copies the placements' identity records into `frame`'s ray-instance table.
    pub(super) fn write_ray_instances(&mut self, frame: usize, placements: &[Placement]) {
        let capacity = self.frames[frame].ray_instance_capacity as usize;
        let Some(buffer) = self.frames[frame].ray_instances.as_mut() else {
            return;
        };
        let Some(dst) = buffer.mapped_bytes() else {
            return;
        };
        let records: Vec<GpuRayInstanceRecord> = placements
            .iter()
            .take(capacity)
            .map(ray_instance_record)
            .collect();
        let bytes = bytemuck::cast_slice(&records);
        dst[..bytes.len()].copy_from_slice(bytes);
    }

    /// Copies the placements' identity records into `frame`'s ray-instance table at the
    /// partitioned structure's own slots, one per placement in `slots`.
    ///
    /// The table is addressed by instance id, and the partitioned structure's ids are its
    /// stable slots rather than a dense range, so the whole table is rewritten with the
    /// unresolvable record first: a slot no live instance holds must read as unmirrored,
    /// not as whatever the instance that used to hold it resolved to.
    pub(super) fn write_ray_instances_at(
        &mut self,
        frame: usize,
        placements: &[Placement],
        slots: &[u32],
    ) {
        let capacity = self.frames[frame].ray_instance_capacity as usize;
        let Some(buffer) = self.frames[frame].ray_instances.as_mut() else {
            return;
        };
        let Some(dst) = buffer.mapped_bytes() else {
            return;
        };
        let mut records = vec![GpuRayInstanceRecord::unmirrored(); capacity];
        for (placement, slot) in placements.iter().zip(slots) {
            let Some(record) = records.get_mut(*slot as usize) else {
                continue;
            };
            *record = ray_instance_record(placement);
        }
        let bytes = bytemuck::cast_slice(&records);
        dst[..bytes.len()].copy_from_slice(bytes);
    }

    /// Ensures `frame`'s instance buffer holds `count` instances (host-visible AS-build
    /// input + BDA), growing to the next power of two.
    pub(super) fn ensure_tlas_capacity(&mut self, frame: usize, count: u32) -> Result<()> {
        if self.frames[frame].instance_buffer.is_some()
            && self.frames[frame].instance_capacity >= count
        {
            return Ok(());
        }
        let mut capacity = self.frames[frame]
            .instance_capacity
            .max(INITIAL_TLAS_CAPACITY);
        while capacity < count {
            capacity *= 2;
        }
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let buffer = Buffer::new(
            &self.resources,
            vk::DeviceSize::from(capacity) * INSTANCE_STRIDE,
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &alloc_info,
        )?;
        self.frames[frame].instance_buffer = Some(buffer);
        self.frames[frame].instance_capacity = capacity;
        Ok(())
    }

    /// Ensures `frame`'s TLAS build scratch is at least `bytes` (device-local, BDA).
    pub(super) fn ensure_tlas_scratch(
        &mut self,
        frame: usize,
        bytes: vk::DeviceSize,
    ) -> Result<()> {
        if self.frames[frame].scratch.is_some()
            && vk::DeviceSize::from(self.frames[frame].scratch_capacity) >= bytes
        {
            return Ok(());
        }
        let buffer = make_scratch_buffer(&self.resources, bytes)?;
        self.frames[frame].scratch = Some(buffer);
        self.frames[frame].scratch_capacity = bytes as u32;
        Ok(())
    }

    /// Ensures `frame`'s shared skinned-BLAS build/refit scratch is at least `bytes`.
    pub(super) fn ensure_blas_scratch(
        &mut self,
        frame: usize,
        bytes: vk::DeviceSize,
    ) -> Result<()> {
        if self.frames[frame].blas_scratch.is_some()
            && vk::DeviceSize::from(self.frames[frame].blas_scratch_capacity) >= bytes
        {
            return Ok(());
        }
        let buffer = make_scratch_buffer(&self.resources, bytes)?;
        self.frames[frame].blas_scratch = Some(buffer);
        self.frames[frame].blas_scratch_capacity = bytes as u32;
        Ok(())
    }

    /// Writes `tlas` into `frame`'s set-6 binding 0 (the mesh fragment's TLAS).
    pub(super) fn write_mesh_set(
        &self,
        device: &Device,
        frame: usize,
        tlas: vk::AccelerationStructureKHR,
    ) {
        let structures = [tlas];
        let mut accel_write = vk::WriteDescriptorSetAccelerationStructureKHR::default()
            .acceleration_structures(&structures);
        let mut write = vk::WriteDescriptorSet::default()
            .dst_set(self.frames[frame].mesh_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR)
            .push_next(&mut accel_write);
        // `descriptor_count` is otherwise inferred from the (absent) image/buffer arrays.
        write.descriptor_count = 1;
        // SAFETY: the ash seam. The set + layout are this renderer's; written on the render
        // thread after the slot's fence is waited (no concurrent host access).
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Builds a 0-instance empty TLAS (synchronous one-off submit) and writes it into every
    /// frame's set 6, so set 6 always references a valid AS before any per-frame build.
    pub(super) fn seed_empty_tlas(&mut self, device: &Device) -> Result<()> {
        // A partitioned device's set 6 takes a partitioned structure — the descriptor type
        // admits nothing else — so the seed is one inert-instance build per frame slot.
        if self.ptlas.is_some() {
            let frames = self.ptlas.as_ref().map_or(0, |ptlas| ptlas.frame_count());
            let mut seeds = Vec::with_capacity(frames);
            for frame in 0..frames {
                let Some(op) = self.ptlas.as_mut().and_then(|ptlas| ptlas.plan_seed(frame)) else {
                    return Err(crate::Error::InvalidUploadData(
                        "partitioned structure seed could not be planned".to_owned(),
                    ));
                };
                seeds.push(op);
            }
            record_and_submit_oneoff(device, |cmd| {
                for op in &seeds {
                    // SAFETY: the extension seam. The buffer is recording and each seed's
                    // addresses reference the structure's own live per-frame buffers.
                    unsafe { op.record(cmd) };
                }
            })?;
            for frame in 0..frames {
                let Some(address) = self
                    .ptlas
                    .as_ref()
                    .and_then(|ptlas| ptlas.frame_address(frame))
                else {
                    continue;
                };
                self.write_mesh_set_ptlas(device, frame, address);
            }
            return Ok(());
        }
        let dispatch = self
            .dispatch
            .clone()
            .expect("accel dispatch present on an RT device");
        let geom = instances_geometry(0);
        let geoms = [geom];
        let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
            .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
            .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
            .geometries(&geoms);
        let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()` (1).
        unsafe {
            dispatch.get_acceleration_structure_build_sizes(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &size_info,
                &[0],
                &mut sizes,
            );
        }
        let empty = AccelerationStructure::create(
            &self.resources,
            &dispatch,
            sizes.acceleration_structure_size.max(256),
            vk::AccelerationStructureTypeKHR::TOP_LEVEL,
        )?;
        let scratch = make_scratch_buffer(&self.resources, sizes.build_scratch_size.max(256))?;
        let scratch_addr = device.buffer_device_address(scratch.handle());
        let dst = empty.handle();
        let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
            .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
            .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
            .dst_acceleration_structure(dst)
            .geometries(&geoms)
            .scratch_data(vk::DeviceOrHostAddressKHR {
                device_address: scratch_addr,
            });
        let range = vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(0);
        let ranges = [range];

        // A synchronous one-off submit on a private transient command pool, then `wait_idle`
        // (an init-time path; never per-frame).
        record_and_submit_oneoff(device, |cmd| {
            // SAFETY: the ash seam. One build info; the range slice length equals its
            // `geometry_count` (1).
            unsafe {
                dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
            }
        })?;
        device.wait_idle()?;
        drop(scratch);

        // Share the one empty TLAS across every slot. A real per-frame build later replaces
        // a slot's TLAS (and rewrites its set) on demand.
        let empty = Arc::new(empty);
        let handle = empty.handle();
        for frame in 0..self.frames.len() {
            self.frames[frame].tlas = Some(Arc::clone(&empty));
            self.write_mesh_set(device, frame, handle);
        }
        Ok(())
    }
}
