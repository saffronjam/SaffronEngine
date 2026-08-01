//! The native overlay submissions: the gizmo, the wind field, and the vegetation debug draws.

use saffron_assets::RendererUploader;

use saffron_rendering::{Renderer, Uploader};
use saffron_scene::CameraView;

use crate::overlay::build_scene_edit_overlay;

use super::*;

impl HostLayer {
    /// Builds the native gizmo overlay geometry and submits it. `edit_chrome` (Edit and not
    /// previewing) gates the gizmo / billboards / frustums / debug overlays; colliders + the
    /// skeleton draw outside it.
    pub(super) fn submit_scene_edit_overlay(
        &mut self,
        renderer: &mut Renderer,
        cam: &CameraView,
        width: u32,
        height: u32,
    ) {
        self.refresh_rejection_overlay();
        self.refresh_heatmap_overlay(renderer);
        self.refresh_wind_overlay(renderer, cam);
        let edit_chrome = self.editor.editor_chrome_visible();
        // The overlay's debug/collider builders resolve meshes through the renderer's uploader
        // + descriptors (the bindless texture binds); the gizmo / billboards / skeleton are
        // pure projection. Skinning is off for the resolve (bounds only, no skin stream).
        let vegetation_cell = self.runtime.vegetation_cell();
        let vegetation = vegetation_cell.borrow();
        let (_, navigation, _, _) = self.runtime.vegetation_control_authorities();
        let (depth_tested, on_top) = match self.uploader.as_ref() {
            Some(uploader) => {
                let gpu = RendererUploader::new(uploader, renderer.descriptors(), false);
                build_scene_edit_overlay(
                    &mut self.editor,
                    &mut self.assets,
                    &gpu,
                    &crate::overlay::OverlayFrame {
                        cam,
                        width,
                        height,
                        edit_chrome,
                        vegetation: vegetation.as_ref(),
                        rejections: &self.rejection_overlay.rows,
                        heatmap: &self.heatmap_overlay.rows,
                        wind: &self.wind_overlay.rows,
                        navigation: Some(navigation),
                    },
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        renderer.submit_overlay(depth_tested, on_top);
    }

    /// Rebuilds the heatmap texels when the manifest identity or the resident-cell
    /// set moves: every resident cell's micro tiles fold to a 16×16 max-density
    /// grid, and each occupied texel drops one straight-down surface cast for its
    /// height. The flag gates all work; texels cap at 4096.
    /// Rebuilds the wind-overlay rows: a 16×16 ground grid (2 m spacing) centred on
    /// the camera, heights re-cast only when the snapped origin moves, velocities
    /// resampled from the composed field every frame. The flag gates all work.
    pub(super) fn refresh_wind_overlay(&mut self, renderer: &mut Renderer, cam: &CameraView) {
        self.wind_overlay.rows.clear();
        if !self.editor.debug_overlays.wind_vectors {
            self.wind_overlay.origin = None;
            return;
        }
        const GRID: i64 = 16;
        const SPACING: f64 = 2.0;
        let eye = cam.view.inverse().col(3);
        let origin = (
            (f64::from(eye.x) / SPACING).floor() as i64 - GRID / 2,
            (f64::from(eye.z) / SPACING).floor() as i64 - GRID / 2,
        );
        if self.wind_overlay.origin != Some(origin) {
            self.wind_overlay.origin = Some(origin);
            self.wind_overlay.heights.clear();
            let Some(uploader) = self.uploader.as_ref() else {
                self.wind_overlay.origin = None;
                return;
            };
            let gpu = RendererUploader::new(uploader, renderer.descriptors(), false);
            let scene = self.editor.active_scene();
            let assets = &mut self.assets;
            let ticks = |meters: f64| (meters * 4096.0).round() as i128;
            for gz in 0..GRID {
                for gx in 0..GRID {
                    let x = (origin.0 + gx) as f64 * SPACING + SPACING * 0.5;
                    let z = (origin.1 + gz) as f64 * SPACING + SPACING * 0.5;
                    let mut height = 0.0_f32;
                    if let Ok(ray_origin) = saffron_spatial::WorldPosition::from_global_ticks([
                        ticks(x),
                        ticks(f64::from(eye.y) + 100.0),
                        ticks(z),
                    ]) && let Ok(ray) = saffron_spatial::SurfaceRay::new(
                        ray_origin,
                        saffron_geometry::glam::DVec3::NEG_Y,
                        1_000.0,
                    ) && let Ok(Some(hit)) =
                        saffron_assets::query_scene_surface_ray(&gpu, scene, assets, &ray)
                    {
                        height = hit.surface.position.world_meters().y as f32;
                    }
                    self.wind_overlay.heights.push(height);
                }
            }
        }
        let profile = self.editor.active_scene().environment.wind.profile();
        let sources = self.editor.active_scene().local_wind_source_field();
        let time = self.editor.simulation_time_s;
        for gz in 0..GRID {
            for gx in 0..GRID {
                let x = (origin.0 + gx) as f64 * SPACING + SPACING * 0.5;
                let z = (origin.1 + gz) as f64 * SPACING + SPACING * 0.5;
                let height = self.wind_overlay.heights[(gz * GRID + gx) as usize];
                let sample = saffron_wind::sample_composed(
                    &profile,
                    &sources,
                    saffron_geometry::glam::DVec3::new(x, f64::from(height), z),
                    time,
                );
                self.wind_overlay
                    .rows
                    .push((glam::Vec3::new(x as f32, height, z as f32), sample.velocity));
            }
        }
    }

    pub(super) fn refresh_heatmap_overlay(&mut self, renderer: &mut Renderer) {
        if !self.editor.debug_overlays.vegetation_heatmap {
            self.heatmap_overlay.rows.clear();
            self.heatmap_overlay.fingerprint = 0;
            return;
        }
        let vegetation_cell = self.runtime.vegetation_cell();
        let vegetation = vegetation_cell.borrow();
        let Some(world) = vegetation.as_ref() else {
            self.heatmap_overlay.rows.clear();
            self.heatmap_overlay.fingerprint = 0;
            return;
        };
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut fingerprint = 0x84222325_cbf29ce4_u64;
        let mut mix = |value: u64| {
            fingerprint ^= value;
            fingerprint = fingerprint.wrapping_mul(FNV_PRIME);
        };
        for chunk in world.manifest_identity().bytes().chunks(8) {
            let mut word = [0_u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            mix(u64::from_be_bytes(word));
        }
        let resident: Vec<_> = world.resident_cells().collect();
        for (cell, _) in &resident {
            let [x, y, z] = cell.coordinates();
            mix(x as u64);
            mix(y as u64);
            mix(z as u64);
            mix(u64::from(cell.level()));
        }
        if fingerprint == self.heatmap_overlay.fingerprint {
            return;
        }
        self.heatmap_overlay.fingerprint = fingerprint;
        self.heatmap_overlay.rows.clear();
        const GRID: usize = 16;
        const TEXEL_CAP: usize = 4096;
        // Fold the resident micro tiles into flat (x, z, top, density) texels first,
        // so the world borrow ends before the surface casts borrow the scene.
        let mut texels: Vec<(f64, f64, f64, f32)> = Vec::new();
        for (cell, generation) in &resident {
            let Some(tiles) = generation.micro_fields() else {
                continue;
            };
            let bounds = cell.bounds();
            let min = bounds.min_ticks();
            let max = bounds.max_ticks_exclusive();
            let span_x = (max[0] - min[0]) as f64 / 4096.0;
            let span_z = (max[2] - min[2]) as f64 / 4096.0;
            let origin_x = min[0] as f64 / 4096.0;
            let origin_z = min[2] as f64 / 4096.0;
            let top_y = max[1] as f64 / 4096.0;
            let mut grid = [[0_u16; GRID]; GRID];
            for tile in tiles {
                let [dim_x, dim_y, dim_z] = tile.dimensions;
                if dim_x == 0 || dim_z == 0 {
                    continue;
                }
                for (gx, column) in grid.iter_mut().enumerate() {
                    for (gz, slot) in column.iter_mut().enumerate() {
                        let tx = gx * dim_x as usize / GRID;
                        let tz = gz * dim_z as usize / GRID;
                        let index = (tx * dim_y as usize) * dim_z as usize + tz;
                        if let Some(density) = tile.density.get(index) {
                            *slot = (*slot).max(*density);
                        }
                    }
                }
            }
            for (gx, column) in grid.iter().enumerate() {
                for (gz, density) in column.iter().enumerate() {
                    if *density == 0 || texels.len() >= TEXEL_CAP {
                        continue;
                    }
                    texels.push((
                        origin_x + (gx as f64 + 0.5) / GRID as f64 * span_x,
                        origin_z + (gz as f64 + 0.5) / GRID as f64 * span_z,
                        top_y,
                        f32::from(*density) / f32::from(u16::MAX),
                    ));
                }
            }
        }
        drop(resident);
        let Some(uploader) = self.uploader.as_ref() else {
            return;
        };
        let gpu = RendererUploader::new(uploader, renderer.descriptors(), false);
        let scene = self.editor.active_scene();
        let assets = &mut self.assets;
        for (x, z, top, density) in texels {
            let ticks = |meters: f64| (meters * 4096.0).round() as i128;
            let Ok(origin) =
                saffron_spatial::WorldPosition::from_global_ticks([ticks(x), ticks(top), ticks(z)])
            else {
                continue;
            };
            let Ok(ray) = saffron_spatial::SurfaceRay::new(
                origin,
                saffron_geometry::glam::DVec3::NEG_Y,
                1_000.0,
            ) else {
                continue;
            };
            if let Ok(Some(hit)) =
                saffron_assets::query_scene_surface_ray(&gpu, scene, assets, &ray)
            {
                let meters = hit.surface.position.world_meters();
                self.heatmap_overlay.rows.push((
                    glam::Vec3::new(meters.x as f32, meters.y as f32, meters.z as f32),
                    density,
                ));
            }
        }
    }

    /// Rebuilds the rejection-overlay marker rows when the manifest identity or the
    /// resident-cell set moves. The flag gates all work; rows cap at 4096 markers.
    pub(super) fn refresh_rejection_overlay(&mut self) {
        if !self.editor.debug_overlays.vegetation_rejections {
            self.rejection_overlay.rows.clear();
            self.rejection_overlay.fingerprint = 0;
            return;
        }
        let vegetation_cell = self.runtime.vegetation_cell();
        let vegetation = vegetation_cell.borrow();
        let Some(world) = vegetation.as_ref() else {
            self.rejection_overlay.rows.clear();
            self.rejection_overlay.fingerprint = 0;
            return;
        };
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64;
        let mut mix = |value: u64| {
            fingerprint ^= value;
            fingerprint = fingerprint.wrapping_mul(FNV_PRIME);
        };
        for chunk in world.manifest_identity().bytes().chunks(8) {
            let mut word = [0_u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            mix(u64::from_be_bytes(word));
        }
        let resident: Vec<_> = world.resident_cells().map(|(cell, _)| cell).collect();
        for cell in &resident {
            let [x, y, z] = cell.coordinates();
            mix(x as u64);
            mix(y as u64);
            mix(z as u64);
            mix(u64::from(cell.level()));
        }
        if fingerprint == self.rejection_overlay.fingerprint {
            return;
        }
        self.rejection_overlay.fingerprint = fingerprint;
        self.rejection_overlay.rows.clear();
        const MARKER_CAP: usize = 4096;
        let by_cell: std::collections::BTreeMap<_, _> = world
            .manifest()
            .cells
            .iter()
            .map(|row| (row.cell, row.artifact_hash))
            .collect();
        let store = self.assets.vegetation_artifact_store();
        let reason_byte = |reason: saffron_runtime::CandidateRejectionReason| -> u8 {
            use saffron_runtime::CandidateRejectionReason as Reason;
            match reason {
                Reason::SurfaceMiss => 0,
                Reason::Threshold => 1,
                Reason::WeightedElimination => 2,
                Reason::PriorityExclusion => 3,
                Reason::Competition => 4,
                Reason::ForeignOwner => 5,
                Reason::NoSpecies => 6,
            }
        };
        for cell in &resident {
            if self.rejection_overlay.rows.len() >= MARKER_CAP {
                break;
            }
            let Some(artifact) = by_cell.get(cell) else {
                continue;
            };
            let Ok(Some(bytes)) = store.read_cell_section(
                *artifact,
                saffron_runtime::VegetationCellSectionKind::RejectionDiagnostics,
            ) else {
                continue;
            };
            let Ok(facet) = saffron_runtime::decode_vegetation_rejection_diagnostics(&bytes) else {
                continue;
            };
            for rejected in &facet.rejected {
                if self.rejection_overlay.rows.len() >= MARKER_CAP {
                    break;
                }
                let ticks = rejected.position.global_ticks();
                self.rejection_overlay.rows.push((
                    glam::Vec3::new(
                        ticks[0] as f32 / 4096.0,
                        ticks[1] as f32 / 4096.0,
                        ticks[2] as f32 / 4096.0,
                    ),
                    reason_byte(rejected.reason),
                ));
            }
        }
    }

    /// Lazily builds the host-owned one-off [`Uploader`] from the renderer's device + queue.
    /// The uploader is `Arc`-rooted in the device resources, so it outlives any single-frame
    /// `&mut Renderer` borrow.
    pub(super) fn ensure_uploader(&mut self, renderer: &Renderer) {
        if self.uploader.is_some() {
            return;
        }
        let queue = renderer.device().graphics_queue.clone();
        match Uploader::new(renderer.device(), &queue) {
            Ok(uploader) => self.uploader = Some(uploader),
            Err(err) => tracing::error!("uploader create failed: {err}"),
        }
    }
}
