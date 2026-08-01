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
            let predicted_eye = view.eye
                + self.eye_velocity(world, view)
                    * saffron_rendering::PAGE_DEMAND_PREDICTION_SECONDS;
            let mut mesh_stats: HashMap<u64, MeshDemand> = HashMap::new();
            if let Some(world_state) = self.worlds.get(&world.0) {
                // Both stores in one loop: a plant's pages fault exactly like a scene
                // instance's, and vegetation holds most of the paged geometry.
                let placed = world_state
                    .instances
                    .iter()
                    .filter_map(|((entity, _), instance)| {
                        let state = scene.world_transform_state(*entity)?;
                        let current = state.current.col(3).truncate();
                        let moved = instance.world_revision != instance.previous_world_revision;
                        let previous = if moved {
                            state.previous.col(3).truncate()
                        } else {
                            current
                        };
                        Some((instance.mesh, current, previous))
                    })
                    .chain(world_state.plants.values().filter_map(|plant| {
                        let GpuSceneTransform::Static(placement) = plant.record.transform else {
                            return None;
                        };
                        // A placed plant is static by construction, so it never leads.
                        let position = placement.to_matrix().col(3).truncate();
                        Some((plant.mesh, position, position))
                    }));
                for (mesh, position, previous) in placed {
                    let approach = closest_approach(position, previous, view, predicted_eye);
                    let clip = view.view_proj * position.extend(1.0);
                    let in_frustum = clip.w > 0.0
                        && clip.x.abs() <= clip.w * 1.2
                        && clip.y.abs() <= clip.w * 1.2;
                    let gi_reachable =
                        position.cmpge(view.gi_min).all() && position.cmple(view.gi_max).all();
                    let stats = mesh_stats.entry(mesh).or_insert(MeshDemand {
                        approach: f32::INFINITY,
                        in_frustum: false,
                        gi_reachable: false,
                    });
                    stats.approach = stats.approach.min(approach);
                    stats.in_frustum |= in_frustum;
                    stats.gi_reachable |= gi_reachable;
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
                // The transition total is a composite Q15.16 error; its silhouette
                // component dominates, so metres is the conservative reading.
                let error_metres = page.transition_error.total as f32 / 65_536.0;
                if let Some(priority) = page_demand_priority(
                    error_metres,
                    page_bounds_radius(page),
                    view.proj_scale,
                    stats,
                    entry.payload_source.as_ref(),
                ) {
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

impl GpuSceneMirror {
    /// This world's smoothed eye velocity, folded from this frame's observation.
    fn eye_velocity(
        &mut self,
        world: GpuSceneWorldId,
        view: saffron_rendering::PageDemandView,
    ) -> Vec3 {
        let motion = self.eye_motion.entry(world.0).or_default();
        // The mirror only differences successive positions, so the anchor cancels; it just has to
        // be the same one every frame.
        let Ok(position) = saffron_spatial::WorldPosition::from_render_relative(
            view.eye,
            saffron_spatial::WorldPosition::origin(),
        ) else {
            motion.reset();
            return Vec3::ZERO;
        };
        motion
            .observe(position, f64::from(view.frame_seconds))
            .as_vec3()
    }
}

/// What one mesh's placements demand of the frontier this frame.
struct MeshDemand {
    /// Closest the instance and the eye come over the prediction horizon.
    approach: f32,
    in_frustum: bool,
    /// Inside the window a GI or reflection ray reaches, whether or not on screen.
    gi_reachable: bool,
}

/// The nearest an instance and the eye come over the prediction horizon: the smaller of where
/// they stand and where both are led to.
///
/// The minimum rather than the predicted distance alone, so a page is asked for as soon as
/// either reading calls for it and a lead that turns out wrong cannot push one away.
fn closest_approach(
    current: Vec3,
    previous: Vec3,
    view: saffron_rendering::PageDemandView,
    predicted_eye: Vec3,
) -> f32 {
    let standing = (current - view.eye).length().max(0.05);
    let predicted = predicted_instance_position(current, previous, view);
    standing.min((predicted - predicted_eye).length().max(0.05))
}

/// The residency priority one frontier page earns, or `None` when its projected error falls
/// under the refinement threshold.
///
/// Screen-space error is the base; what reads the page and how long its payload takes to arrive
/// are what rank two pages of equal error against each other.
fn page_demand_priority(
    error_metres: f32,
    radius: f32,
    proj_scale: f32,
    stats: &MeshDemand,
    source: Option<&PagePayloadSource>,
) -> Option<u64> {
    let distance = (stats.approach - radius).max(0.05);
    let projected = error_metres * proj_scale / distance
        * page_demand_reach_weight(stats.in_frustum, stats.gi_reachable)
        * page_demand_source_lead(source);
    (projected > 0.25).then(|| {
        (projected * 1024.0).min(saffron_rendering::PAGE_DEMAND_PREDICTED_CEILING as f32) as u64
    })
}

/// Where an instance stands at the end of the prediction horizon, from the travel its
/// `previous`→`current` world transforms recorded over `frame_seconds`.
fn predicted_instance_position(
    current: Vec3,
    previous: Vec3,
    view: saffron_rendering::PageDemandView,
) -> Vec3 {
    if view.frame_seconds <= 0.0 || !view.frame_seconds.is_finite() {
        return current;
    }
    let velocity = (current - previous) / view.frame_seconds;
    if !velocity.is_finite() {
        return current;
    }
    current + velocity * saffron_rendering::PAGE_DEMAND_PREDICTION_SECONDS
}

/// Lead multiplier for how long a page's payload takes to arrive once requested.
///
/// A cooked hierarchy is already in memory, so its page materializes almost immediately. An
/// artifact page is a file read plus an envelope decode on the stream worker, so it has to be
/// asked for further out to land at the same time — ranking both by projected error alone starves
/// the slower source exactly when the frontier is contended.
fn page_demand_source_lead(source: Option<&PagePayloadSource>) -> f32 {
    match source {
        Some(PagePayloadSource::Artifact(_)) => 2.0,
        Some(PagePayloadSource::Cooked(_)) | None => 1.0,
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

    fn demand_view(frame_seconds: f32) -> saffron_rendering::PageDemandView {
        saffron_rendering::PageDemandView {
            eye: Vec3::ZERO,
            proj_scale: 1000.0,
            view_proj: saffron_geometry::glam::Mat4::IDENTITY,
            gi_min: Vec3::splat(-1000.0),
            gi_max: Vec3::splat(1000.0),
            frame_seconds,
        }
    }

    #[test]
    fn an_approaching_instance_is_scored_where_it_will_be() {
        let view = demand_view(0.05);
        // 20 m/s straight at the eye over a 50 ms frame.
        let predicted = predicted_instance_position(Vec3::X * 100.0, Vec3::X * 101.0, view);
        assert!(
            predicted.x < 100.0 - 5.0,
            "a closing instance must be scored ahead of where it stands, got {predicted:?}"
        );
        let receding = predicted_instance_position(Vec3::X * 100.0, Vec3::X * 99.0, view);
        assert!(receding.x > 100.0, "a receding instance leads outward too");
        assert_eq!(
            predicted_instance_position(Vec3::X * 100.0, Vec3::X * 101.0, demand_view(0.0)),
            Vec3::X * 100.0,
            "with no measured interval there is no velocity to lead with"
        );
    }

    #[test]
    fn each_world_measures_its_own_eye() {
        let eye_at = |x: f32| saffron_rendering::PageDemandView {
            eye: Vec3::X * x,
            ..demand_view(0.05)
        };
        let scene = GpuSceneWorldId(0);
        let preview = GpuSceneWorldId(1);

        let mut interrupted = GpuSceneMirror::default();
        interrupted.eye_velocity(scene, eye_at(0.0));
        // One mirror serves every world, so a thumbnail render lands between two scene frames
        // with a completely different eye. Reading it as the scene camera's travel would clamp
        // as a teleport and restart the estimate at rest.
        interrupted.eye_velocity(preview, eye_at(5_000.0));
        let after_excursion = interrupted.eye_velocity(scene, eye_at(1.0));

        let mut alone = GpuSceneMirror::default();
        alone.eye_velocity(scene, eye_at(0.0));
        let uninterrupted = alone.eye_velocity(scene, eye_at(1.0));

        assert!(
            uninterrupted.x > 0.0,
            "a camera closing on +x must measure a velocity at all, got {uninterrupted:?}"
        );
        assert_eq!(
            after_excursion, uninterrupted,
            "another world's eye must not reach this world's estimate"
        );
    }

    #[test]
    fn a_stationary_instance_scores_at_its_standing_distance() {
        let position = Vec3::new(3.0, -4.0, 12.0);
        assert_eq!(
            predicted_instance_position(position, position, demand_view(0.016)),
            position
        );
    }

    #[test]
    fn an_artifact_page_is_asked_for_further_out_than_a_cooked_one() {
        let artifact = PagePayloadSource::Artifact(crate::ByteSource {
            path: "pages.bin".to_owned(),
            offset: 0,
            length: 0,
        });
        let cooked = PagePayloadSource::Cooked(std::sync::Arc::new(
            saffron_geometry::PortableVirtualHierarchy::default(),
        ));
        assert!(
            page_demand_source_lead(Some(&artifact)) > page_demand_source_lead(Some(&cooked)),
            "a file read plus decode has to be requested earlier than an in-memory build"
        );
        assert_eq!(
            page_demand_source_lead(Some(&cooked)),
            page_demand_source_lead(None),
            "an unknown source must not be given a lead it has not earned"
        );
    }

    fn demand(approach: f32) -> MeshDemand {
        MeshDemand {
            approach,
            in_frustum: true,
            gi_reachable: true,
        }
    }

    #[test]
    fn the_ranked_score_carries_the_lead_and_the_reach_of_the_page_it_scores() {
        let artifact = PagePayloadSource::Artifact(crate::ByteSource {
            path: "pages.bin".to_owned(),
            offset: 0,
            length: 0,
        });
        let cooked = PagePayloadSource::Cooked(std::sync::Arc::new(
            saffron_geometry::PortableVirtualHierarchy::default(),
        ));
        let score = |stats: &MeshDemand, source: Option<&PagePayloadSource>| {
            page_demand_priority(0.02, 1.0, 1000.0, stats, source)
        };
        // Same page, same distance, same error: the payload source is the only difference, so a
        // score that ignored it would rank these equal.
        assert!(
            score(&demand(40.0), Some(&artifact)) > score(&demand(40.0), Some(&cooked)),
            "the slower source has to be asked for further out"
        );
        // Same page again, this time differing only in what reads it.
        let unread = MeshDemand {
            in_frustum: false,
            gi_reachable: false,
            ..demand(40.0)
        };
        assert!(
            score(&demand(40.0), Some(&cooked)) > score(&unread, Some(&cooked)),
            "a page nothing reads must rank under one on screen"
        );
        // Far enough out that even the artifact lead leaves it under the threshold.
        assert_eq!(score(&demand(4_000.0), Some(&artifact)), None);
    }

    #[test]
    fn a_closing_instance_is_ranked_from_where_it_closes_to() {
        let view = demand_view(0.05);
        // 20 m/s straight at a stationary eye: the lead is what the score sees, not the standing
        // distance.
        let approach = closest_approach(Vec3::X * 100.0, Vec3::X * 101.0, view, view.eye);
        assert!(approach < 100.0 - 5.0, "got {approach}");
        // A lead that points away must not push the page out: the standing distance still counts.
        let receding = closest_approach(Vec3::X * 100.0, Vec3::X * 99.0, view, view.eye);
        assert!((receding - 100.0).abs() < 0.001, "got {receding}");
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
