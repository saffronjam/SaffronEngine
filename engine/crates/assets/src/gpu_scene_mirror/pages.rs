use super::*;

impl GpuSceneMirror {
    /// Drives page-payload streaming for one frame: drains completed loads into the
    /// residency authority (patching child-handle tables), scores the refinement
    /// frontier against the view (projected transition error, frustum probability,
    /// motion), and feeds the load worker within its in-flight budget.
    ///
    /// # Errors
    ///
    /// Propagates payload patch failures; a per-page load failure is reported to the
    /// residency authority and logged, never fatal.
    pub fn drive_page_streaming(
        &mut self,
        world: GpuSceneWorldId,
        scene: &Scene,
        view: Option<saffron_rendering::PageDemandView>,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        for result in self.worker.drain() {
            let Some(entry) = self.shared.meshes.get(&result.mesh) else {
                target.residency.fail_load(result.handle);
                continue;
            };
            match result.payload {
                Ok(mut payload) => {
                    let mut resolved = true;
                    for (index, cook_child) in payload.child_pages.clone().iter().enumerate() {
                        match entry.pages.get(*cook_child as usize) {
                            Some(page) => payload
                                .patch_child(index, page.device)
                                .map_err(Error::Render)?,
                            None => {
                                resolved = false;
                                break;
                            }
                        }
                    }
                    if resolved {
                        target.residency.complete_load(result.handle, payload.bytes);
                    } else {
                        target.residency.fail_load(result.handle);
                    }
                }
                Err(err) => {
                    tracing::warn!("page stream: {err}");
                    target.residency.fail_load(result.handle);
                }
            }
        }

        if let Some(view) = view {
            struct MeshDemand {
                distance: f32,
                in_frustum: bool,
                /// Inside the window a GI or reflection ray reaches, whether or not on screen.
                gi_reachable: bool,
                moved: bool,
            }
            let mut mesh_stats: HashMap<u64, MeshDemand> = HashMap::new();
            if let Some(world_state) = self.worlds.get(&world.0) {
                // Both stores in one loop: a plant's pages fault exactly like a scene
                // instance's, and vegetation holds most of the paged geometry.
                let placed = world_state
                    .instances
                    .iter()
                    .filter_map(|((entity, _), instance)| {
                        let state = scene.world_transform_state(*entity)?;
                        Some((
                            instance.mesh,
                            state.current.col(3).truncate(),
                            instance.world_revision != instance.previous_world_revision,
                        ))
                    })
                    .chain(world_state.plants.values().filter_map(|plant| {
                        let GpuSceneTransform::Static(placement) = plant.record.transform else {
                            return None;
                        };
                        // A placed plant is static by construction, so it never scores the
                        // motion boost.
                        Some((plant.mesh, placement.to_matrix().col(3).truncate(), false))
                    }));
                for (mesh, position, moved) in placed {
                    let distance = (position - view.eye).length().max(0.05);
                    let clip = view.view_proj * position.extend(1.0);
                    let in_frustum = clip.w > 0.0
                        && clip.x.abs() <= clip.w * 1.2
                        && clip.y.abs() <= clip.w * 1.2;
                    let gi_reachable =
                        position.cmpge(view.gi_min).all() && position.cmple(view.gi_max).all();
                    let stats = mesh_stats.entry(mesh).or_insert(MeshDemand {
                        distance: f32::INFINITY,
                        in_frustum: false,
                        gi_reachable: false,
                        moved: false,
                    });
                    stats.distance = stats.distance.min(distance);
                    stats.in_frustum |= in_frustum;
                    stats.gi_reachable |= gi_reachable;
                    stats.moved |= moved;
                }
            }
            for handle in target.residency.frontier() {
                let Some((mesh, cook)) = self.shared.page_lookup.get(&handle.index) else {
                    continue;
                };
                let Some(stats) = mesh_stats.get(mesh) else {
                    continue;
                };
                let Some(entry) = self.shared.meshes.get(mesh) else {
                    continue;
                };
                let Some(page) = entry.mesh.hierarchy_pages.get(*cook as usize) else {
                    continue;
                };
                let radius = page_bounds_radius(page);
                // The transition total is a composite Q15.16 error; its silhouette
                // component dominates, so metres is the conservative reading.
                let error_metres = page.transition_error.total as f32 / 65_536.0;
                let distance = (stats.distance - radius).max(0.05);
                let mut projected = error_metres * view.proj_scale / distance;
                projected *= page_demand_reach_weight(stats.in_frustum, stats.gi_reachable);
                if stats.moved {
                    projected *= 2.0;
                }
                if projected > 0.25 {
                    let priority = (projected * 1024.0)
                        .min(saffron_rendering::PAGE_DEMAND_PREDICTED_CEILING as f32)
                        as u64;
                    target.residency.demand(handle, priority);
                }
            }
        }

        let budget = PAGE_STREAM_MAX_IN_FLIGHT.saturating_sub(self.worker.in_flight());
        if budget > 0 {
            let mut requests = Vec::new();
            for handle in target.residency.take_load_requests(budget) {
                let Some((mesh, cook)) = self.shared.page_lookup.get(&handle.index).copied() else {
                    target.residency.fail_load(handle);
                    continue;
                };
                let source = self
                    .shared
                    .meshes
                    .get(&mesh)
                    .and_then(|entry| entry.payload_source.clone());
                let Some(source) = source else {
                    target.residency.fail_load(handle);
                    continue;
                };
                requests.push(PageLoadRequest {
                    mesh,
                    page_id: cook,
                    handle,
                    source,
                });
            }
            self.worker.enqueue(requests);
        }
        Ok(())
    }
}

/// How much of a page's projected error survives, given what actually reads it this frame.
///
/// Three tiers rather than on-screen or not: a page feeding a cone march or a reflection is
/// genuinely read even when nothing on screen shows it. On-screen content still wins outright,
/// because a missing page there is a visible hole rather than a soft gather.
fn page_demand_reach_weight(in_frustum: bool, gi_reachable: bool) -> f32 {
    if in_frustum {
        1.0
    } else if gi_reachable {
        0.5
    } else {
        0.25
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use saffron_rendering::validation_issue_count;

    #[test]
    fn page_payloads_stream_to_residency_through_the_worker() {
        let Some(mut harness) = harness("stream") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9601);
        write_triangle_mesh(&mut harness.assets, mesh_id, "stream-tri");

        let mut scene = Scene::new();
        let entity = scene.create_entity("Streamed");
        scene
            .add_component(entity, MeshComponent { mesh: mesh_id })
            .unwrap();
        harness.sync(&mut scene);
        assert!(
            harness.residency.stats().registered >= 1,
            "mirrored pages register with residency"
        );

        // Guaranteed roots are demanded at registration; each drive hands requests to
        // the worker, drains its results, and publishes ready payloads.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            harness.drive(&scene);
            if harness.residency.stats().resident >= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "page stream stalled: {:?}",
                harness.residency.stats()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let stats = harness.residency.stats();
        assert!(stats.resident_bytes > 0, "payload bytes accounted");
        let published = harness
            .gpu_data
            .page_table
            .iter()
            .filter(|(_, record)| record.byte_length > 0)
            .count();
        assert!(
            published >= 1,
            "the published page record carries its payload span"
        );

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn gi_reachable_pages_outrank_pages_nothing_reads() {
        // The ordering that matters, as an inequality rather than three magic numbers:
        // on-screen beats GI-reachable beats unreachable.
        let visible = page_demand_reach_weight(true, false);
        let gi_only = page_demand_reach_weight(false, true);
        let neither = page_demand_reach_weight(false, false);
        assert!(visible > gi_only, "on-screen content must win outright");
        assert!(
            gi_only > neither,
            "a page a march reads must outrank one nothing reads"
        );
        // On-screen wins whether or not it is also reachable: a missing page there is a visible
        // hole, not a soft gather, so the two flags must not compound into a higher tier.
        assert_eq!(visible, page_demand_reach_weight(true, true));
    }
}
