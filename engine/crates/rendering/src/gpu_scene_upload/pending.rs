use std::sync::Arc;

use crate::global_gpu_data::{
    FrameUploadRing, GlobalGpuData, GlobalGpuTableKind, GpuArenaRange, GpuBufferUpload, GpuHandle,
};
use crate::gpu_types::MaterialParamsData;
use crate::render_graph::RenderGraph;
use crate::{Device, Result};

/// One queued arena byte upload from the asset mirror.
pub enum GpuArenaUploadRequest {
    /// Vertex bytes into [`GlobalGpuData::vertices`] at `range`.
    Vertices {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The retained CPU vertex stream.
        data: Arc<[saffron_geometry::Vertex]>,
    },
    /// Index bytes into [`GlobalGpuData::indices`] at `range`.
    Indices {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The retained CPU index stream.
        data: Arc<[u32]>,
    },
    /// One material parameter block into [`GlobalGpuData::material_parameters`] at `range`.
    MaterialParams {
        /// Destination element range (one block).
        range: GpuArenaRange,
        /// The packed std430 parameter block.
        data: Box<MaterialParamsData>,
    },
    /// Submesh records into [`GlobalGpuData::submesh_table`] at `range`.
    Submeshes {
        /// Destination element range.
        range: GpuArenaRange,
        /// The geometry's submesh records.
        data: Vec<crate::GpuSubmeshRecord>,
    },
    /// One resident page's payload bytes into [`GlobalGpuData::pages`] at `range`.
    PageBytes {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The locked page payload.
        data: Vec<u8>,
    },
    /// One geometry's assembly-part table into [`GlobalGpuData::parts`] at `range` —
    /// the prototype records followed by the use records.
    Parts {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The packed prototype + use records.
        data: Vec<u8>,
    },
    /// One cell's packed micro field tiles into [`GlobalGpuData::fields`] at `range`.
    Fields {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The packed tile headers + density samples.
        data: Vec<u8>,
    },
    /// Deformation-provider records into [`GlobalGpuData::deformation_providers`].
    DeformationProviders {
        /// Destination element range.
        range: GpuArenaRange,
        /// The provider records.
        data: Vec<crate::GpuDeformationProviderRecord>,
    },
    /// Provider parameter words into [`GlobalGpuData::deformation_parameters`].
    DeformationParameters {
        /// Destination element range.
        range: GpuArenaRange,
        /// The parameter words.
        data: Vec<u32>,
    },
}

/// Pending resident-table and arena uploads queued by the asset mirror, drained into
/// graph transfer passes by [`record_pending_global_uploads`].
#[derive(Default)]
pub struct GpuScenePendingUploads {
    records: Vec<(GlobalGpuTableKind, GpuHandle)>,
    retires: Vec<(GlobalGpuTableKind, GpuHandle)>,
    arenas: Vec<GpuArenaUploadRequest>,
}

impl GpuScenePendingUploads {
    /// Queues one live record's bytes for staging.
    pub fn stage_record(&mut self, kind: GlobalGpuTableKind, handle: GpuHandle) {
        self.records.push((kind, handle));
    }

    /// Queues one record's fence-safe retirement and GPU tombstone.
    pub fn retire_record(&mut self, kind: GlobalGpuTableKind, handle: GpuHandle) {
        self.retires.push((kind, handle));
    }

    /// Queues one arena byte upload.
    pub fn upload_arena(&mut self, request: GpuArenaUploadRequest) {
        self.arenas.push(request);
    }

    /// Total queued operations.
    pub fn len(&self) -> usize {
        self.records.len() + self.retires.len() + self.arenas.len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.retires.is_empty() && self.arenas.is_empty()
    }
}

/// Drains the pending resident-table stages, arena uploads, and retirements into graph
/// transfer passes, growing device buffers first.
pub fn record_pending_global_uploads(
    pending: &mut GpuScenePendingUploads,
    device: &Device,
    graph: &mut RenderGraph,
    gpu_data: &mut GlobalGpuData,
    frame_slot: usize,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let enqueue_growth =
        |growth: Option<crate::GpuArenaGrowth>, name: &'static str, graph: &mut RenderGraph| {
            if let Some(growth) = growth {
                growth.enqueue(graph, device, name);
            }
        };
    enqueue_growth(
        gpu_data.prototypes.prepare_growth(device)?,
        "prototypes grow",
        graph,
    );
    enqueue_growth(
        gpu_data.geometries.prepare_growth(device)?,
        "geometries grow",
        graph,
    );
    enqueue_growth(
        gpu_data.materials.prepare_growth(device)?,
        "materials grow",
        graph,
    );
    enqueue_growth(
        gpu_data.textures.prepare_growth(device)?,
        "textures grow",
        graph,
    );
    enqueue_growth(
        gpu_data.coverage.prepare_growth(device)?,
        "coverage grow",
        graph,
    );
    enqueue_growth(
        gpu_data.skeletons.prepare_growth(device)?,
        "skeletons grow",
        graph,
    );
    enqueue_growth(
        gpu_data.sdfs.prepare_growth(device)?,
        "sdf-table grow",
        graph,
    );
    enqueue_growth(
        gpu_data.page_table.prepare_growth(device)?,
        "page-table grow",
        graph,
    );
    enqueue_growth(
        gpu_data.vertices.prepare_growth(device)?,
        "vertices grow",
        graph,
    );
    enqueue_growth(
        gpu_data.indices.prepare_growth(device)?,
        "indices grow",
        graph,
    );
    enqueue_growth(
        gpu_data.material_parameters.prepare_growth(device)?,
        "material-parameters grow",
        graph,
    );
    enqueue_growth(
        gpu_data.submesh_table.prepare_growth(device)?,
        "submesh-table grow",
        graph,
    );
    enqueue_growth(gpu_data.pages.prepare_growth(device)?, "pages grow", graph);
    enqueue_growth(gpu_data.parts.prepare_growth(device)?, "parts grow", graph);
    enqueue_growth(
        gpu_data.fields.prepare_growth(device)?,
        "fields grow",
        graph,
    );
    enqueue_growth(
        gpu_data.deformed_vertices.prepare_growth(device)?,
        "deformed-vertices grow",
        graph,
    );
    enqueue_growth(
        gpu_data.prev_deformed_vertices.prepare_growth(device)?,
        "prev-deformed-vertices grow",
        graph,
    );
    enqueue_growth(
        gpu_data.deformation_providers.prepare_growth(device)?,
        "deformation-providers grow",
        graph,
    );
    enqueue_growth(
        gpu_data.deformation_parameters.prepare_growth(device)?,
        "deformation-parameters grow",
        graph,
    );

    let GlobalGpuData {
        prototypes,
        geometries,
        materials,
        textures,
        coverage,
        skeletons,
        sdfs,
        page_table,
        vertices,
        indices,
        material_parameters,
        submesh_table,
        pages,
        parts,
        fields,
        deformation_providers,
        deformation_parameters,
        uploads,
        ..
    } = gpu_data;

    for (kind, handle) in pending.records.drain(..) {
        let upload = match kind {
            GlobalGpuTableKind::Prototype => stage_if_live(prototypes, uploads, frame_slot, handle),
            GlobalGpuTableKind::Geometry => stage_if_live(geometries, uploads, frame_slot, handle),
            GlobalGpuTableKind::Material => stage_if_live(materials, uploads, frame_slot, handle),
            GlobalGpuTableKind::Texture => stage_if_live(textures, uploads, frame_slot, handle),
            GlobalGpuTableKind::Coverage => stage_if_live(coverage, uploads, frame_slot, handle),
            GlobalGpuTableKind::Skeleton => stage_if_live(skeletons, uploads, frame_slot, handle),
            GlobalGpuTableKind::Sdf => stage_if_live(sdfs, uploads, frame_slot, handle),
            GlobalGpuTableKind::Page => stage_if_live(page_table, uploads, frame_slot, handle),
        }?;
        if let Some(upload) = upload {
            upload.enqueue(graph, device, "global-table record");
        }
    }

    for request in pending.arenas.drain(..) {
        let upload = match request {
            GpuArenaUploadRequest::Vertices { range, data } => {
                vertices.stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?
            }
            GpuArenaUploadRequest::Indices { range, data } => {
                indices.stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?
            }
            GpuArenaUploadRequest::MaterialParams { range, data } => {
                material_parameters.stage(uploads, frame_slot, range, bytemuck::bytes_of(&*data))?
            }
            GpuArenaUploadRequest::Submeshes { range, data } => {
                submesh_table.stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?
            }
            GpuArenaUploadRequest::PageBytes { range, data } => {
                pages.stage(uploads, frame_slot, range, &data)?
            }
            GpuArenaUploadRequest::Parts { range, data } => {
                parts.stage(uploads, frame_slot, range, &data)?
            }
            GpuArenaUploadRequest::Fields { range, data } => {
                fields.stage(uploads, frame_slot, range, &data)?
            }
            GpuArenaUploadRequest::DeformationProviders { range, data } => deformation_providers
                .stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?,
            GpuArenaUploadRequest::DeformationParameters { range, data } => deformation_parameters
                .stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?,
        };
        upload.enqueue(graph, device, "global-arena bytes");
    }

    for (kind, handle) in pending.retires.drain(..) {
        let tombstone = match kind {
            GlobalGpuTableKind::Prototype => prototypes
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Geometry => geometries
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Material => materials
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Texture => textures
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Coverage => coverage
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Skeleton => skeletons
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Sdf => sdfs
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Page => page_table
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
        };
        if let Some(tombstone) = tombstone {
            tombstone.enqueue(graph, device, "global-table tombstone");
        }
    }
    Ok(())
}

fn stage_if_live<T: bytemuck::Pod, K>(
    table: &crate::ResidentGpuTable<T, K>,
    uploads: &mut FrameUploadRing,
    frame_slot: usize,
    handle: GpuHandle,
) -> Result<Option<GpuBufferUpload>> {
    if table.get(handle).is_none() {
        return Ok(None);
    }
    table.stage(uploads, frame_slot, handle).map(Some)
}
