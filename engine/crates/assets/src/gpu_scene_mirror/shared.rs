use super::*;

impl SharedMirror {
    pub(super) fn next_revision(&mut self) -> u64 {
        self.content_revision += 1;
        self.content_revision
    }

    /// Makes the mesh's shared records resident, returning whether the mesh resolved.
    pub(super) fn ensure_mesh(
        &mut self,
        id: u64,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<bool> {
        if self.meshes.contains_key(&id) {
            return Ok(true);
        }
        let Some(mesh) = assets.load_mesh_asset(gpu, Uuid(id)) else {
            return Ok(false);
        };
        if mesh.hierarchy_pages.is_empty() {
            tracing::warn!("gpu scene mirror: mesh {id} carries no hierarchy pages; skipped");
            return Ok(false);
        }
        let entry = self.build_mesh_entry(id, mesh, assets, gpu, target)?;
        self.meshes.insert(id, entry);
        Ok(true)
    }

    pub(super) fn build_mesh_entry(
        &mut self,
        id: u64,
        mesh: Arc<GpuMesh>,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<MeshEntry> {
        let InsertedGeometry {
            geometry,
            vertex_range,
            index_range,
            submesh_range,
            parts_range,
        } = insert_geometry(&mesh, target)?;
        let (pages, root_page) = insert_pages(&mesh.hierarchy_pages, self.generation, target)?;
        for (cook_id, page) in pages.iter().enumerate() {
            self.page_lookup
                .insert(page.device.index, (id, cook_id as u32));
        }
        let payload_source = assets.page_payload_source(Uuid(id));
        if payload_source.is_none() {
            tracing::warn!("gpu scene mirror: mesh {id} has no page payload source");
        }

        let slot_count = mesh
            .submeshes
            .iter()
            .map(|submesh| submesh.material_slot + 1)
            .max()
            .unwrap_or(1)
            .max(1);
        let default_key = MaterialKey::default_key();
        let default_handle = self.intern_material(&default_key, assets, gpu, target, None)?;
        for _ in 0..slot_count {
            self.ref_material(&default_key);
        }
        let materials: Arc<[GpuSceneMaterialHandle]> =
            std::iter::repeat_n(default_handle, slot_count as usize).collect();

        let bounds = prototype_bounds(&mesh);
        let mechanics = packed_mechanics(assets.plant_family_mechanics(Uuid(id)));
        let sdfs = insert_mesh_sdfs(&mesh, self.generation, target)?;
        let result = target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                GpuScenePrototypeRecord {
                    geometry,
                    materials,
                    deformation: None,
                    sdfs: sdfs.iter().map(|sdf| sdf.scene).collect(),
                    root_page,
                    bounds,
                    page_bounds: prototype_page_bounds(&mesh),
                    source_generation: self.generation,
                    flags: 0,
                    mechanics,
                },
            ))
            .map_err(gpu_scene_error)?;
        let GpuSceneSharedDeltaResult::PrototypeCreated(prototype) = result else {
            unreachable!("create prototype returns PrototypeCreated");
        };
        Ok(MeshEntry {
            mesh,
            geometry,
            vertex_range,
            index_range,
            submesh_range,
            parts_range,
            pages,
            payload_source,
            sdfs,
            prototype,
            slot_count,
            mechanics,
            refs: 0,
        })
    }

    /// Swaps a refreshed mesh's geometry, pages, bounds, and slot table in place while
    /// keeping the prototype handle stable for every live instance.
    pub(super) fn replace_mesh_content(
        &mut self,
        id: u64,
        mesh: Arc<GpuMesh>,
        payload_source: Option<PagePayloadSource>,
        mechanics: [u32; 4],
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let InsertedGeometry {
            geometry,
            vertex_range,
            index_range,
            submesh_range,
            parts_range,
        } = insert_geometry(&mesh, target)?;
        let (pages, root_page) = insert_pages(&mesh.hierarchy_pages, self.generation, target)?;
        for (cook_id, page) in pages.iter().enumerate() {
            self.page_lookup
                .insert(page.device.index, (id, cook_id as u32));
        }

        let new_slot_count = mesh
            .submeshes
            .iter()
            .map(|submesh| submesh.material_slot + 1)
            .max()
            .unwrap_or(1)
            .max(1);

        let entry = self.meshes.get(&id).expect("refreshed mesh entry present");
        let prototype = entry.prototype;
        let old_slot_count = entry.slot_count;
        let old_geometry = entry.geometry;
        let old_vertex_range = entry.vertex_range;
        let old_index_range = entry.index_range;
        let old_submesh_range = entry.submesh_range;
        let old_parts_range = entry.parts_range;
        let old_pages: Vec<MeshPage> = entry.pages.clone();

        let default_key = MaterialKey::default_key();
        let default_handle = self.materials[&default_key].scene_handle;
        let materials: Arc<[GpuSceneMaterialHandle]> =
            std::iter::repeat_n(default_handle, new_slot_count as usize).collect();
        let bounds = prototype_bounds(&mesh);

        let old_sdfs: Vec<MeshSdf> = entry.sdfs.clone();
        if let Some(entry) = self.meshes.get_mut(&id) {
            entry.mechanics = mechanics;
        }
        let sdfs = insert_mesh_sdfs(&mesh, self.generation, target)?;
        target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::UpdatePrototype {
                handle: prototype,
                record: GpuScenePrototypeRecord {
                    geometry,
                    materials,
                    deformation: None,
                    sdfs: sdfs.iter().map(|sdf| sdf.scene).collect(),
                    root_page,
                    bounds,
                    page_bounds: prototype_page_bounds(&mesh),
                    source_generation: self.generation,
                    flags: 0,
                    mechanics,
                },
            })
            .map_err(gpu_scene_error)?;
        // The refreshed record does not reference the superseded fields, so they retire now.
        for sdf in old_sdfs.iter().rev() {
            target
                .gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::RemoveSdf(sdf.scene))
                .map_err(gpu_scene_error)?;
            target
                .pending
                .retire_record(GlobalGpuTableKind::Sdf, sdf.device);
        }

        for page in old_pages.iter().rev() {
            target
                .gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::RemovePage(page.scene))
                .map_err(gpu_scene_error)?;
            target
                .residency
                .unregister_page(page.device, target.gpu_data)?;
            self.page_lookup.remove(&page.device.index);
            target
                .pending
                .retire_record(GlobalGpuTableKind::Page, page.device);
        }
        target
            .pending
            .retire_record(GlobalGpuTableKind::Geometry, old_geometry);
        target.gpu_data.vertices.retire(old_vertex_range)?;
        target.gpu_data.indices.retire(old_index_range)?;
        target.gpu_data.submesh_table.retire(old_submesh_range)?;
        if old_parts_range.count != 0 {
            target.gpu_data.parts.retire(old_parts_range)?;
        }

        if new_slot_count > old_slot_count {
            for _ in old_slot_count..new_slot_count {
                self.ref_material(&default_key);
            }
        } else {
            for _ in new_slot_count..old_slot_count {
                self.unref_material(&default_key, target)?;
            }
        }

        let entry = self
            .meshes
            .get_mut(&id)
            .expect("refreshed mesh entry present");
        entry.mesh = mesh;
        entry.geometry = geometry;
        entry.vertex_range = vertex_range;
        entry.index_range = index_range;
        entry.submesh_range = submesh_range;
        entry.parts_range = parts_range;
        entry.pages = pages;
        entry.sdfs = sdfs;
        entry.payload_source = payload_source;
        entry.slot_count = new_slot_count;
        Ok(())
    }

    /// Removes a mesh's prototype, pages, and geometry records. Callers guarantee no
    /// instance references the prototype.
    pub(super) fn remove_mesh_records(
        &mut self,
        id: u64,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let Some(entry) = self.meshes.remove(&id) else {
            return Ok(());
        };
        target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::RemovePrototype(entry.prototype))
            .map_err(gpu_scene_error)?;
        for sdf in entry.sdfs.iter().rev() {
            target
                .gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::RemoveSdf(sdf.scene))
                .map_err(gpu_scene_error)?;
            target
                .pending
                .retire_record(GlobalGpuTableKind::Sdf, sdf.device);
        }
        for page in entry.pages.iter().rev() {
            target
                .gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::RemovePage(page.scene))
                .map_err(gpu_scene_error)?;
            target
                .residency
                .unregister_page(page.device, target.gpu_data)?;
            self.page_lookup.remove(&page.device.index);
            target
                .pending
                .retire_record(GlobalGpuTableKind::Page, page.device);
        }
        target
            .pending
            .retire_record(GlobalGpuTableKind::Geometry, entry.geometry);
        target.gpu_data.vertices.retire(entry.vertex_range)?;
        target.gpu_data.indices.retire(entry.index_range)?;
        target.gpu_data.submesh_table.retire(entry.submesh_range)?;
        if entry.parts_range.count != 0 {
            target.gpu_data.parts.retire(entry.parts_range)?;
        }
        let default_key = MaterialKey::default_key();
        for _ in 0..entry.slot_count {
            self.unref_material(&default_key, target)?;
        }
        Ok(())
    }

    pub(super) fn ref_mesh(&mut self, id: u64) {
        if let Some(entry) = self.meshes.get_mut(&id) {
            entry.refs += 1;
        }
    }

    /// Releases one instance reference; the entry stays interned for reuse until the
    /// asset itself is deleted or the shared state rebuilds.
    pub(super) fn unref_mesh(&mut self, id: u64) {
        if let Some(entry) = self.meshes.get_mut(&id) {
            entry.refs = entry.refs.saturating_sub(1);
        }
    }

    pub(super) fn intern_material(
        &mut self,
        key: &MaterialKey,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
        atlas: Option<&Arc<saffron_rendering::GpuTexture>>,
    ) -> Result<GpuSceneMaterialHandle> {
        if let Some(entry) = self.materials.get(key) {
            return Ok(entry.scene_handle);
        }
        let overrides = key.overrides_value();
        let asset = assets.resolve_slot_material(Uuid(key.material), &overrides);
        let mut submesh = assets.resolve_material_asset(gpu, &asset);
        // A family that cooked an atlas has UVs addressing it, so the slot's own image is no
        // longer what those UVs index — the packed atlas is. Substituting here keeps the whole
        // material path (params, codegen, coverage) identical for atlased and un-atlased families.
        if let Some(atlas) = atlas {
            submesh.albedo_texture = Some(Arc::clone(atlas));
            submesh.coverage_texture = Some(Arc::clone(atlas));
        }
        let submesh = submesh;
        let codegen_shader = assets.codegen_shader_for(Uuid(key.material));
        let device = self.build_material_device_records(
            key,
            &asset,
            &submesh,
            codegen_shader.as_deref(),
            target,
        )?;
        let source_revision = self.next_revision();
        let result = target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                GpuSceneMaterialRecord {
                    table: device.table,
                    source_revision,
                },
            ))
            .map_err(gpu_scene_error)?;
        let GpuSceneSharedDeltaResult::MaterialCreated(scene_handle) = result else {
            unreachable!("create material returns MaterialCreated");
        };
        self.materials.insert(
            key.clone(),
            MaterialEntry {
                device,
                scene_handle,
                refs: 0,
            },
        );
        Ok(scene_handle)
    }

    /// Re-resolves one interned material's content behind its stable persistent-scene
    /// handle: new device records replace the old ones, which retire.
    pub(super) fn refresh_material_content(
        &mut self,
        key: &MaterialKey,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        if !self.materials.contains_key(key) {
            return Ok(());
        }
        let overrides = key.overrides_value();
        let asset = assets.resolve_slot_material(Uuid(key.material), &overrides);
        let submesh = assets.resolve_material_asset(gpu, &asset);
        let codegen_shader = assets.codegen_shader_for(Uuid(key.material));
        let device = self.build_material_device_records(
            key,
            &asset,
            &submesh,
            codegen_shader.as_deref(),
            target,
        )?;
        let source_revision = self.next_revision();

        let entry = self.materials.get_mut(key).expect("entry present");
        target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::UpdateMaterial {
                handle: entry.scene_handle,
                record: GpuSceneMaterialRecord {
                    table: device.table,
                    source_revision,
                },
            })
            .map_err(gpu_scene_error)?;
        let old = std::mem::replace(&mut entry.device, device);
        self.retire_material_device_records(old, target)
    }

    pub(super) fn build_material_device_records(
        &mut self,
        key: &MaterialKey,
        asset: &MaterialAsset,
        submesh: &SubmeshMaterial,
        codegen_shader: Option<&str>,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<MaterialDeviceRecords> {
        let mut textures = Vec::new();
        let albedo_arc = submesh
            .albedo_texture
            .clone()
            .unwrap_or_else(|| Arc::clone(target.default_white));
        let albedo = self.intern_texture(&albedo_arc, target)?;
        textures.push(albedo_arc.bindless_index());
        let normal = match submesh.normal_texture.as_ref() {
            Some(texture) => {
                let handle = self.intern_texture(texture, target)?;
                textures.push(texture.bindless_index());
                handle
            }
            None => GpuHandle::INVALID,
        };
        let coverage_texture = match submesh.coverage_texture.as_ref() {
            Some(texture) => {
                let handle = self.intern_texture(texture, target)?;
                textures.push(texture.bindless_index());
                Some((handle, texture.extent.width, texture.extent.height))
            }
            None => None,
        };

        let coverage_handle = coverage_texture.map_or(albedo, |(handle, ..)| handle);
        let coverage_extent = coverage_texture.map_or_else(
            || {
                [
                    albedo_arc.extent.width.max(1),
                    albedo_arc.extent.height.max(1),
                ]
            },
            |(_, width, height)| [width.max(1), height.max(1)],
        );
        let coverage =
            build_coverage_record(key, asset, submesh, coverage_handle, coverage_extent)?;
        let coverage = match coverage {
            Some(record) => {
                let handle = target.gpu_data.coverage.insert(record)?;
                target
                    .pending
                    .stage_record(GlobalGpuTableKind::Coverage, handle);
                Some(handle)
            }
            None => None,
        };

        let (parameters, _) = target.gpu_data.material_parameters.allocate(1, 1)?;
        // The temporal coverage phase is per-frame state, not table content; the packed
        // block carries phase zero.
        let mut pinned = Vec::new();
        let (params, ..) = resolve_material_params(submesh, DEFAULT_WHITE_SLOT, 0, &mut pinned);
        target
            .pending
            .upload_arena(GpuArenaUploadRequest::MaterialParams {
                range: parameters,
                data: Box::new(params),
            });
        let pack = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        let table = target.gpu_data.materials.insert(GpuMaterialTableRecord {
            base_color_texture: albedo,
            normal_texture: normal,
            coverage: coverage.unwrap_or(GpuHandle::INVALID),
            parameter_index: parameters.first,
            material_class: material_class(submesh, asset.unlit),
            proxy_albedo: pack(submesh.base_color.x)
                | (pack(submesh.base_color.y) << 8)
                | (pack(submesh.base_color.z) << 16),
            occupancy: submesh.thin_sheet.as_ref().map_or(1.0, |sheet| {
                crate::render_material::derive_parity_occupancy(
                    sheet.transmission.to_array(),
                    sheet.thickness,
                )
            }),
            shader_index: codegen_shader.map_or(0, |shader| {
                target.gpu_data.executor_shaders.register(shader)
            }),
            flags: 0,
        })?;
        target
            .pending
            .stage_record(GlobalGpuTableKind::Material, table);
        Ok(MaterialDeviceRecords {
            table,
            coverage,
            parameters,
            textures,
        })
    }

    pub(super) fn retire_material_device_records(
        &mut self,
        records: MaterialDeviceRecords,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        target
            .pending
            .retire_record(GlobalGpuTableKind::Material, records.table);
        if let Some(coverage) = records.coverage {
            target
                .pending
                .retire_record(GlobalGpuTableKind::Coverage, coverage);
        }
        target
            .gpu_data
            .material_parameters
            .retire(records.parameters)?;
        for index in records.textures {
            self.unref_texture(index, target);
        }
        Ok(())
    }

    pub(super) fn ref_material(&mut self, key: &MaterialKey) {
        if let Some(entry) = self.materials.get_mut(key) {
            entry.refs += 1;
        }
    }

    pub(super) fn unref_material(
        &mut self,
        key: &MaterialKey,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let remove = {
            let Some(entry) = self.materials.get_mut(key) else {
                return Ok(());
            };
            entry.refs = entry.refs.saturating_sub(1);
            entry.refs == 0
        };
        if remove {
            self.remove_material_records(key, target)?;
        }
        Ok(())
    }

    pub(super) fn remove_material_records(
        &mut self,
        key: &MaterialKey,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let Some(entry) = self.materials.remove(key) else {
            return Ok(());
        };
        target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::RemoveMaterial(entry.scene_handle))
            .map_err(gpu_scene_error)?;
        self.retire_material_device_records(entry.device, target)
    }

    pub(super) fn intern_texture(
        &mut self,
        texture: &Arc<GpuTexture>,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<GpuHandle> {
        let index = texture.bindless_index();
        if let Some(entry) = self.textures.get_mut(&index) {
            entry.refs += 1;
            return Ok(entry.handle);
        }
        let handle = target.gpu_data.textures.insert(GpuTextureTableRecord {
            descriptor_index: index,
            width: texture.extent.width,
            height: texture.extent.height,
            mip_count: texture.mip_count,
            flags: 0,
            reserved: [0; 3],
        })?;
        target
            .pending
            .stage_record(GlobalGpuTableKind::Texture, handle);
        self.textures.insert(
            index,
            TextureEntry {
                handle,
                texture: Arc::clone(texture),
                refs: 1,
            },
        );
        Ok(handle)
    }

    pub(super) fn unref_texture(&mut self, index: u32, target: &mut GpuSceneMirrorTarget<'_>) {
        let remove = {
            let Some(entry) = self.textures.get_mut(&index) else {
                return;
            };
            entry.refs = entry.refs.saturating_sub(1);
            entry.refs == 0
        };
        if remove && let Some(entry) = self.textures.remove(&index) {
            target
                .pending
                .retire_record(GlobalGpuTableKind::Texture, entry.handle);
            drop(entry.texture);
        }
    }
}

/// Packs a plant family's authored response into the prototype record's four words, in
/// the cooked integer forms so the GPU reads exactly what the cooker wrote. All zero for
/// anything that is not a plant family, which the wind prepass reads as "derive the
/// response from the plant's height alone".
pub(crate) fn packed_mechanics(
    mechanics: Option<saffron_vegetation::MechanicalResponse>,
) -> [u32; 4] {
    let Some(mechanics) = mechanics else {
        return [0; 4];
    };
    [
        mechanics.stiffness.bits() as u32,
        mechanics.drag.bits() as u32,
        mechanics.flutter.bits() as u32,
        u32::from(mechanics.damping.bits()) | (u32::from(mechanics.bend_limit.bits()) << 16),
    ]
}

fn prototype_bounds(mesh: &GpuMesh) -> [f32; 4] {
    let center = (mesh.bounds_min + mesh.bounds_max) * 0.5;
    let radius = (mesh.bounds_max - center).length();
    [center.x, center.y, center.z, radius.max(0.0)]
}

/// The SWEPT local bounds of each cooked LEAF page — one triangle cluster or one aggregate brick
/// each, in page order.
///
/// The deformed extent rather than the static one: consumers dirty shadow pages from these, and a
/// static box would leave a swaying branch's new position un-re-rendered.
///
/// Leaves only, because an interior page's bounds ENCLOSE its whole subtree: including them would
/// add the prototype's own root box to every dirty set and make the tight cluster boxes beside it
/// pointless. Dropping them stays conservative — a coarse representation's vertices are convex
/// combinations of the fine ones it simplifies, so it lies inside the union of its children's
/// boxes, which is what the cook's own subtree closure asserts.
fn prototype_page_bounds(mesh: &GpuMesh) -> Arc<[GpuScenePageBounds]> {
    leaf_page_bounds(&mesh.hierarchy_pages)
}

/// [`prototype_page_bounds`] over a bare page directory: a page is a leaf when no other page
/// depends on it, which is the only place the cooked directory records the tree.
fn leaf_page_bounds(pages: &[PortableHierarchyPage]) -> Arc<[GpuScenePageBounds]> {
    let interior: HashSet<u32> = pages.iter().filter_map(|page| page.dependency).collect();
    pages
        .iter()
        .filter(|page| !interior.contains(&page.id))
        .map(|page| {
            let scale = |bits: i32| bits as f32 / 65_536.0;
            GpuScenePageBounds {
                min: page.deformed_bounds.min_bits.map(scale),
                max: page.deformed_bounds.max_bits.map(scale),
            }
        })
        .collect()
}

/// The device ranges one mesh's geometry occupies in the global arenas.
struct InsertedGeometry {
    geometry: GpuHandle,
    vertex_range: GpuArenaRange,
    index_range: GpuArenaRange,
    submesh_range: GpuArenaRange,
    parts_range: GpuArenaRange,
}

fn insert_geometry(
    mesh: &GpuMesh,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<InsertedGeometry> {
    let vertex_stride = size_of::<Vertex>() as u32;
    let vertex_bytes = mesh
        .vertex_count
        .checked_mul(vertex_stride)
        .ok_or_else(|| mirror_error("mesh vertex stream exceeds the global vertex arena"))?;
    let index_bytes = mesh
        .index_count
        .checked_mul(4)
        .ok_or_else(|| mirror_error("mesh index stream exceeds the global index arena"))?;
    let (vertex_range, _) = target.gpu_data.vertices.allocate(vertex_bytes, 16)?;
    let (index_range, _) = target.gpu_data.indices.allocate(index_bytes, 4)?;
    let submesh_records: Vec<GpuSubmeshRecord> = if mesh.submeshes.is_empty() {
        vec![GpuSubmeshRecord {
            first_index: 0,
            index_count: mesh.index_count,
            material_slot: 0,
            reserved: 0,
        }]
    } else {
        mesh.submeshes
            .iter()
            .map(|submesh| GpuSubmeshRecord {
                first_index: submesh.first_index,
                index_count: submesh.index_count,
                material_slot: submesh.material_slot,
                reserved: 0,
            })
            .collect()
    };
    let submesh_count = u32::try_from(submesh_records.len())
        .map_err(|_| mirror_error("mesh submesh table exceeds the submesh arena"))?;
    let (submesh_range, _) = target.gpu_data.submesh_table.allocate(submesh_count, 1)?;
    // An assembly (a multi-prototype plant family) packs its prototype + use records
    // into the parts arena; the prototype count rides the geometry's reserved word so
    // the shaders can split the two tables.
    let (parts_range, prototype_count) = match &mesh.assembly {
        Some(assembly) => {
            let byte_len = u32::try_from(assembly.byte_len())
                .map_err(|_| mirror_error("mesh assembly table exceeds the parts arena"))?;
            let (range, _) = target.gpu_data.parts.allocate(byte_len, 16)?;
            let prototype_count = u32::try_from(assembly.prototypes.len())
                .map_err(|_| mirror_error("mesh assembly table exceeds the parts arena"))?;
            (range, prototype_count)
        }
        None => (EMPTY_RANGE, 0),
    };
    let geometry = target.gpu_data.geometries.insert(GpuGeometryRecord {
        vertices: vertex_range,
        indices: index_range,
        clusters: EMPTY_RANGE,
        parts: parts_range,
        voxels: EMPTY_RANGE,
        submeshes: submesh_range,
        flags: 0,
        vertex_stride,
        index_stride: 4,
        reserved: prototype_count,
    })?;
    if let Some(assembly) = &mesh.assembly {
        target.pending.upload_arena(GpuArenaUploadRequest::Parts {
            range: parts_range,
            data: assembly.packed_bytes(),
        });
    }
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::Submeshes {
            range: submesh_range,
            data: submesh_records,
        });
    target
        .pending
        .stage_record(GlobalGpuTableKind::Geometry, geometry);
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::Vertices {
            range: vertex_range,
            data: Arc::clone(&mesh.cpu_vertices),
        });
    target.pending.upload_arena(GpuArenaUploadRequest::Indices {
        range: index_range,
        data: Arc::clone(&mesh.cpu_indices),
    });
    Ok(InsertedGeometry {
        geometry,
        vertex_range,
        index_range,
        submesh_range,
        parts_range,
    })
}

/// Publishes one mesh's baked signed distance fields: a device-table record per field
/// (the grid placement + atlas identity the occluder scatter reads) and a scene
/// reference each, returned in [`GpuMesh::sdfs`] order for the prototype's range.
fn insert_mesh_sdfs(
    mesh: &GpuMesh,
    generation: u32,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<Vec<MeshSdf>> {
    let mut sdfs = Vec::with_capacity(mesh.sdfs().len());
    for field in mesh.sdfs() {
        let uvec = |a: [u32; 3], w: u32| [a[0], a[1], a[2], w];
        let device = target
            .gpu_data
            .sdfs
            .insert(saffron_rendering::GpuSdfTableRecord {
                local_min: [
                    field.bounds_min.x,
                    field.bounds_min.y,
                    field.bounds_min.z,
                    field.max_dist,
                ],
                local_max: [
                    field.bounds_max.x,
                    field.bounds_max.y,
                    field.bounds_max.z,
                    0.0,
                ],
                voxel_dims: uvec(field.voxel_dims, field.bindless_index()),
                indirection_dims: uvec(field.indirection_dims, field.mip_count),
                atlas_bricks: uvec(field.atlas_bricks, field.proxy_albedo),
            })
            .map_err(Error::Render)?;
        target.pending.stage_record(GlobalGpuTableKind::Sdf, device);
        let result = target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateSdf(
                saffron_rendering::GpuSceneSdfRecord {
                    resource: device,
                    source_revision: u64::from(generation),
                },
            ))
            .map_err(gpu_scene_error)?;
        let GpuSceneSharedDeltaResult::SdfCreated(scene) = result else {
            unreachable!("create sdf returns SdfCreated");
        };
        sdfs.push(MeshSdf { scene, device });
    }
    Ok(sdfs)
}

fn insert_pages(
    hierarchy_pages: &[PortableHierarchyPage],
    generation: u32,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<(Vec<MeshPage>, GpuScenePageHandle)> {
    let mut ordered: Vec<&PortableHierarchyPage> = hierarchy_pages.iter().collect();
    ordered.sort_by_key(|page| page.id);
    let mut by_id: HashMap<u32, MeshPage> = HashMap::new();
    let mut pages = Vec::with_capacity(ordered.len());
    let mut root_page = None;
    for page in ordered {
        let parent = match page.dependency {
            Some(dependency) => Some(*by_id.get(&dependency).ok_or_else(|| {
                mirror_error(format!(
                    "hierarchy page {} depends on unseen page {dependency}",
                    page.id
                ))
            })?),
            None => None,
        };
        let flags = if page.guaranteed_root {
            GPU_PAGE_FLAG_GUARANTEED_ROOT
        } else {
            0
        };
        let device = target.gpu_data.page_table.insert(GpuPageRecord {
            parent: parent.map_or(GpuHandle::INVALID, |p| p.device),
            dependencies: EMPTY_RANGE,
            byte_offset: 0,
            byte_length: 0,
            resident_generation: generation,
            flags,
            reserved: 0,
        })?;
        target
            .pending
            .stage_record(GlobalGpuTableKind::Page, device);
        target
            .residency
            .register_page(device, parent.map(|p| p.device), page.guaranteed_root);
        let result = target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                table: device,
                parent: parent.map(|p| p.scene),
                source_generation: generation,
                flags,
            }))
            .map_err(gpu_scene_error)?;
        let GpuSceneSharedDeltaResult::PageCreated(scene_handle) = result else {
            unreachable!("create page returns PageCreated");
        };
        let entry = MeshPage {
            device,
            scene: scene_handle,
        };
        if root_page.is_none() && page.guaranteed_root {
            root_page = Some(scene_handle);
        }
        by_id.insert(page.id, entry);
        pages.push(entry);
    }
    let root_page = root_page
        .or_else(|| pages.first().map(|page| page.scene))
        .ok_or_else(|| mirror_error("hierarchy has no pages"))?;
    Ok((pages, root_page))
}

/// Builds the canonical coverage record for a material, or `None` for a surface the
/// classifier never samples (standard opaque and standard alpha-blended surfaces).
fn build_coverage_record(
    key: &MaterialKey,
    asset: &MaterialAsset,
    submesh: &SubmeshMaterial,
    texture: GpuHandle,
    extent: [u32; 2],
) -> Result<Option<GpuCoverageRecord>> {
    if let MaterialSurface::ThinSheetFoliage(params) = &asset.surface {
        return Ok(Some(GpuCoverageRecord::from_metadata(
            texture,
            &params.coverage_source,
            &params.coverage,
            params.opacity_micromap,
        )));
    }
    if submesh.blend_mode != BlendMode::Masked {
        return Ok(None);
    }
    let cutoff = f64::from(submesh.alpha_cutoff.clamp(0.0, 1.0));
    let metadata = CoverageMipMetadata {
        reference_cutoff: UnitInterval::from_f64(cutoff)
            .map_err(|error| mirror_error(format!("alpha cutoff out of range: {error}")))?,
        source_extent: extent,
        spatial_hash_salt: coverage_salt(key),
        classification: AlphaClassification::Masked,
        mip_hashes: Vec::new(),
    };
    Ok(Some(GpuCoverageRecord::from_metadata(
        texture,
        &CoverageSource::AlbedoAlpha,
        &metadata,
        OpacityMicromapDerivation::default(),
    )))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use saffron_rendering::validation_issue_count;

    #[test]
    pub(super) fn assembly_mesh_mirrors_its_parts_range_and_prototype_count() {
        let Some(mut harness) = harness("assembly") else {
            return;
        };
        let before = validation_issue_count();
        let (flat, hierarchy) = assembly_family_fixture();
        let gpu = RendererUploader::new(
            &harness.fixture.uploader,
            &harness.fixture.descriptors,
            false,
        );
        let uploaded = gpu
            .upload_mesh(
                &flat,
                &hierarchy,
                &[],
                None,
                saffron_rendering::SdfSource::None,
            )
            .expect("assembly upload");
        let assembly = uploaded.assembly.as_ref().expect("assembly table");
        let expected_bytes = u32::try_from(assembly.byte_len()).unwrap();

        // Register the family the way the plant loader does, then mirror an instance.
        let family_id = Uuid(9_777);
        harness.assets.register_family_render(
            family_id,
            Arc::clone(&uploaded),
            Arc::new(hierarchy),
            mechanics_fixture(),
        );
        let mut scene = Scene::new();
        let entity = scene.create_entity("Family");
        scene
            .add_component(entity, MeshComponent { mesh: family_id })
            .unwrap();
        harness.sync(&mut scene);

        let entry = &harness.mirror.shared.meshes[&family_id.value()];
        assert_eq!(entry.parts_range.count, expected_bytes);
        let record = harness
            .gpu_data
            .geometries
            .get(entry.geometry)
            .expect("geometry record");
        assert_eq!(
            record.reserved, 2,
            "prototype count rides the reserved word"
        );
        assert_eq!(record.parts, entry.parts_range);

        // The family's authored response reaches its prototype record, where the wind
        // prepass reads it.
        let mechanics = mechanics_fixture();
        assert_eq!(
            harness
                .gpu_scene
                .prototype(entry.prototype)
                .expect("prototype record")
                .mechanics,
            [
                mechanics.stiffness.bits() as u32,
                mechanics.drag.bits() as u32,
                mechanics.flutter.bits() as u32,
                u32::from(mechanics.damping.bits())
                    | (u32::from(mechanics.bend_limit.bits()) << 16),
            ],
        );

        drop(uploaded);
        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    /// The dirty-bounds list a moved instance dirties shadow pages from is the prototype's LEAF
    /// pages — one cluster or brick each — and nothing above them.
    ///
    /// An interior page's cooked bounds enclose its whole subtree, so a directory walked whole
    /// hands the consumer the prototype's own root box beside every tight cluster box. The union
    /// is then the root box and the split buys nothing: the sparse gap between a trunk's clusters
    /// and a canopy's re-rasterizes anyway. This test dies the moment an interior page rejoins the
    /// list, because the root's twelve-metre span appears in the output.
    #[test]
    fn only_leaf_pages_contribute_dirty_bounds() {
        let page = |id: u32, dependency: Option<u32>, min: i32, max: i32| PortableHierarchyPage {
            id,
            dependency,
            node: id,
            bounds: saffron_geometry::PortableBounds {
                min_bits: [min << 16, 0, 0],
                max_bits: [max << 16, 1 << 16, 1 << 16],
            },
            deformed_bounds: saffron_geometry::PortableBounds {
                min_bits: [min << 16, 0, 0],
                max_bits: [max << 16, 1 << 16, 1 << 16],
            },
            transition_error: saffron_geometry::AppearanceError::default(),
            guaranteed_root: dependency.is_none(),
        };
        // A trunk cluster at the origin, a canopy cluster ten metres away, and the root page whose
        // bounds enclose both — the shape `close_subtree_bounds` always produces.
        let pages = [
            page(0, None, -1, 11),
            page(1, Some(0), -1, 1),
            page(2, Some(0), 9, 11),
        ];
        let bounds = leaf_page_bounds(&pages);
        assert_eq!(
            bounds.len(),
            2,
            "the root page is not a dirty-bounds source"
        );
        let spans: Vec<f32> = bounds
            .iter()
            .map(|box_| box_.max[0] - box_.min[0])
            .collect();
        assert_eq!(spans, vec![2.0, 2.0], "each box is one cluster wide");
        assert!(
            !bounds
                .iter()
                .any(|box_| box_.min[0] <= -1.0 && box_.max[0] >= 11.0),
            "no box spans the whole prototype"
        );
        // A prototype that cooked one node is that node: it depends on nothing and nothing
        // depends on it, so a leaf filter that keyed on `guaranteed_root` would drop it entirely.
        let single = leaf_page_bounds(&[page(0, None, -1, 1)]);
        assert_eq!(
            single.len(),
            1,
            "a single-page prototype still dirties itself"
        );
    }
}
