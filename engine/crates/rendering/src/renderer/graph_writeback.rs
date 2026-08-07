use super::*;

impl Renderer {
    /// Reads each persistent volume's resolved exit layout back out of the executed graph and
    /// advances the temporal state that rides with it (DDGI probe rotation, the GDF toroidal
    /// recenter, the froxel ping-pong, and the cloud fields).
    pub(super) fn resolve_frame_volume_layouts(
        &mut self,
        graph: &RenderGraph,
        ddgi: &DdgiResult,
        gdf: &GdfResult,
        froxel_slots: Option<(usize, usize, usize)>,
        aerial_slot: Option<usize>,
        cloud_slots: &CloudGraphResult,
    ) {
        // Read back the DDGI images' resolved exit layouts, then advance the temporal state (the
        // ray-set index, the scroll base, the history-reset flag) — only when the chain ran.
        if let Some(slot) = ddgi.rays_slot {
            self.ddgi.set_rays_state(graph.external_state(slot));
        }
        if let Some(slot) = ddgi.irradiance_slot {
            self.ddgi.set_irradiance_state(graph.external_state(slot));
        }
        if let Some(slot) = ddgi.distance_slot {
            self.ddgi.set_distance_state(graph.external_state(slot));
        }
        if ddgi.irradiance.is_some() {
            self.ddgi.advance_frame();
        }
        // Read back the GDF cascade volumes' resolved exit layouts, then commit the toroidal
        // recenter state (prev centers, history, round-robin frame) — only when the chain ran.
        if gdf.cascades.is_some() {
            for c in 0..crate::GDF_CASCADES {
                if let Some(slot) = gdf.cascade_slots[c as usize] {
                    self.global_sdf
                        .set_cascade_layout(c, graph.external_state(slot).layout);
                }
                if let Some(slot) = gdf.occupancy_slots[c as usize] {
                    self.global_sdf
                        .set_occupancy_layout(c, graph.external_state(slot).layout);
                }
            }
            if let Some(slot) = gdf.albedo_slot {
                self.global_sdf
                    .set_albedo_layout(graph.external_state(slot).layout);
            }
            self.global_sdf.advance_frame();
        }
        if let Some(cull) = gdf.cull_state {
            self.global_sdf
                .set_cull_buffer_state(cull.frame, graph.external_buffer_state(cull.slot));
        }

        // Read back the froxel fog volumes' resolved exit layouts and advance the ping-pong write
        // index: the just-written volume becomes next frame's reprojection history.
        if let Some((write_slot, history_slot, integration_slot)) = froxel_slots {
            self.froxel
                .set_scatter_write_layout(graph.external_state(write_slot).layout);
            self.froxel
                .set_scatter_history_layout(graph.external_state(history_slot).layout);
            self.froxel
                .set_integration_layout(graph.external_state(integration_slot).layout);
            self.froxel.advance_frame();
        }

        // Read back the aerial-perspective volume's resolved exit layout.
        if let Some(slot) = aerial_slot {
            self.aerial
                .set_volume_layout(graph.external_state(slot).layout);
        }

        if let Some(slot) = cloud_slots.base {
            self.clouds
                .set_base_layout(graph.external_state(slot).layout);
        }
        if let Some(slot) = cloud_slots.detail {
            self.clouds
                .set_detail_layout(graph.external_state(slot).layout);
        }
        if let Some(slot) = cloud_slots.curl {
            self.clouds
                .set_curl_layout(graph.external_state(slot).layout);
        }
        if let Some(slot) = cloud_slots.weather {
            self.clouds
                .set_weather_layout(graph.external_state(slot).layout);
        }
        if let Some(slot) = cloud_slots.shadow {
            self.clouds
                .set_shadow_layout(graph.external_state(slot).layout);
        }
        {
            let view = &mut self.views[self.active_view.index()];
            for (index, slot) in cloud_slots.reduced.iter().enumerate() {
                if let Some(slot) = slot {
                    view.cloud_reduced[index]
                        .as_mut()
                        .expect("cloud reduced built")
                        .set_graph_state(graph.external_state(*slot));
                }
            }
            if let Some(slot) = cloud_slots.reduced_depth {
                view.cloud_reduced_depth
                    .as_mut()
                    .expect("cloud reduced depth built")
                    .set_graph_state(graph.external_state(slot));
            }
            if let Some(slot) = cloud_slots.full_depth {
                view.cloud_full_depth
                    .as_mut()
                    .expect("cloud full depth built")
                    .set_graph_state(graph.external_state(slot));
            }
            if let Some(slot) = cloud_slots.full_color {
                view.cloud_full_color
                    .as_mut()
                    .expect("cloud full color built")
                    .set_graph_state(graph.external_state(slot));
            }
        }
    }
}
