use super::*;

/// Picks the nearest entity the camera ray strikes: a per-mesh AABB broad-phase rejects far
/// meshes, then a ray-triangle narrow-phase finds the true surface hit (so a click through
/// the empty space inside a loose bounding box misses).
///
/// Covers static [`MeshComponent`] (rest verts transformed by the entity's world matrix) and
/// [`SkinnedMesh`] (verts CPU-skinned through a freshly-rebuilt joint palette into world
/// space, exactly as the GPU does). `ndc` is the click point in clip space `[-1, 1]` matching
/// the rendered image (Y-flipped proj). Returns [`Entity::NULL`] on a miss.
///
/// Takes the upload seam + the active `(width, height)` viewport directly rather than a
/// full [`SceneRenderer`] — picking needs only the AABB mesh upload + the aspect ratio, not
/// the per-frame render driver — so the control plane drives it through
/// `ControlRenderer::with_gpu_uploader` + the viewport-size query.
pub fn pick_entity(
    gpu: &dyn GpuUploader,
    viewport: (u32, u32),
    scene: &mut Scene,
    assets: &mut AssetServer,
    camera: &CameraView,
    ndc: Vec2,
) -> crate::Result<Entity> {
    Ok(
        pick_scene_surface(gpu, viewport, scene, assets, camera, ndc)?
            .map_or(Entity::NULL, |hit| hit.entity),
    )
}

/// Picks the nearest rendered surface hit for a viewport NDC point.
pub fn pick_scene_surface(
    gpu: &dyn GpuUploader,
    viewport: (u32, u32),
    scene: &mut Scene,
    assets: &mut AssetServer,
    camera: &CameraView,
    ndc: Vec2,
) -> crate::Result<Option<SceneSurfaceHit>> {
    let (width, height) = viewport;
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let ray = viewport_ray(viewport, camera, ndc);
    let query = SurfaceRay::new(
        WorldPosition::from_render_relative(ray.origin, WorldPosition::origin())?,
        ray.dir.as_dvec3(),
        f64::from(f32::MAX),
    )?;
    query_scene_surface_ray(gpu, scene, assets, &query)
}

/// Queries every live mesh provider with one arbitrary-direction world ray.
pub fn query_scene_surface_ray(
    gpu: &dyn GpuUploader,
    scene: &mut Scene,
    assets: &mut AssetServer,
    query: &SurfaceRay,
) -> crate::Result<Option<SceneSurfaceHit>> {
    let world_origin = query.origin.to_render_relative(WorldPosition::origin())?;
    let world_ray = Ray {
        origin: world_origin,
        dir: query.direction.as_vec3(),
    };

    // The world transforms come from the last frame's flatten (lockstep with the draw loop);
    // the joint palette is rebuilt fresh below.
    let mut statics: Vec<(Entity, MeshComponent)> = Vec::new();
    scene.for_each::<(&Transform, &MeshComponent), _>(|entity, (_, mesh)| {
        statics.push((entity, *mesh));
    });
    let mut skins: Vec<(Entity, SkinnedMesh)> = Vec::new();
    scene.for_each::<(&Transform, &SkinnedMesh), _>(|entity, (_, skin)| {
        skins.push((entity, skin.clone()));
    });

    let mut nearest: Option<SceneSurfaceHit> = None;

    for (entity, mesh) in statics {
        // A placement ghost must never be its own placement target.
        if scene.has_component::<PreviewGhost>(entity) {
            continue;
        }
        let Some(provider) = static_mesh_surface_provider(gpu, scene, assets, entity, mesh)? else {
            continue;
        };
        if let Some(surface) = provider.raycast(query)? {
            let candidate = SceneSurfaceHit {
                entity,
                capabilities: provider.descriptor().capabilities,
                surface,
            };
            if scene_surface_is_nearer(&candidate, nearest.as_ref()) {
                nearest = Some(candidate);
            }
        }
    }

    for (entity, skin) in skins {
        if scene.has_component::<PreviewGhost>(entity) {
            continue;
        }
        let Some(mesh_ref) = assets.load_mesh_asset(gpu, skin.mesh) else {
            continue;
        };
        if mesh_ref.cpu_vertices.is_empty() || mesh_ref.cpu_skin.is_empty() {
            continue;
        }
        let palette = scene.joint_matrices(&skin);
        if palette.is_empty() {
            continue;
        }
        // Conservative broad-phase: union the bind-pose box through every joint (mirrors the
        // skinned scene-bounds fit). Cheap enough to reject before paying for CPU skinning.
        let mut world_min = Vec3::splat(f32::MAX);
        let mut world_max = Vec3::splat(f32::MIN);
        for joint in &palette {
            world_aabb_from_corners(
                joint,
                mesh_ref.bounds_min,
                mesh_ref.bounds_max,
                &mut world_min,
                &mut world_max,
            );
        }
        if ray_aabb_slab(&world_ray, world_min, world_max).is_none() {
            continue;
        }
        // Skin every vertex into world space once: deformed = Σ w_k · (palette · pos);
        // matches skin.slang, so picking agrees with what the screen shows.
        let deformed: Vec<Vec3> = mesh_ref
            .cpu_vertices
            .iter()
            .zip(&mesh_ref.cpu_skin)
            .map(|(vertex, inf)| {
                let mut acc = Vec3::ZERO;
                for k in 0..4 {
                    let w = inf.weights[k];
                    let j = inf.joints[k] as usize;
                    if w == 0.0 || j >= palette.len() {
                        continue;
                    }
                    acc += w * palette[j].transform_point3(vertex.position);
                }
                acc
            })
            .collect();
        let material_assets =
            assets.resolve_entity_material_assets(scene, entity, &mesh_ref.submeshes);
        let coverage =
            canonical_cpu_coverage(assets, &material_assets, gpu.coverage_temporal_phase());
        if let Some((triangle_index, triangle_hit)) = nearest_triangle_filtered(
            &world_ray,
            &deformed,
            &mesh_ref.cpu_indices,
            |triangle_index, hit| {
                let base = triangle_index as usize * 3;
                let indices = &mesh_ref.cpu_indices[base..base + 3];
                let vertices = [
                    mesh_ref.cpu_vertices[indices[0] as usize],
                    mesh_ref.cpu_vertices[indices[1] as usize],
                    mesh_ref.cpu_vertices[indices[2] as usize],
                ];
                let weights = hit.barycentric;
                let uv = vertices[0].uv0 * weights[0]
                    + vertices[1].uv0 * weights[1]
                    + vertices[2].uv0 * weights[2];
                let anchor = vertices[0].position * weights[0]
                    + vertices[1].position * weights[1]
                    + vertices[2].position * weights[2];
                crate::mesh_surface::coverage_for_triangle(
                    &mesh_ref.submeshes,
                    &coverage,
                    triangle_index,
                )
                .is_none_or(|coverage| coverage.covered(uv, anchor))
            },
        ) {
            let distance_m = f64::from(triangle_hit.distance);
            if distance_m > query.max_distance_m {
                continue;
            }
            let Some(provider_id) = scene
                .component::<IdComponent>(entity)
                .ok()
                .map(|id| SurfaceProviderId(id.id.value()))
            else {
                continue;
            };
            let material_tags = surface_material_tags(scene, entity);
            let revision = mesh_surface_revision(
                skin.mesh.value(),
                &mesh_ref.cpu_vertices,
                &mesh_ref.cpu_indices,
                &palette,
                material_tags.as_slice(),
            );
            let base = triangle_index as usize * 3;
            let indices = &mesh_ref.cpu_indices[base..base + 3];
            let vertices = [
                mesh_ref.cpu_vertices[indices[0] as usize],
                mesh_ref.cpu_vertices[indices[1] as usize],
                mesh_ref.cpu_vertices[indices[2] as usize],
            ];
            let points = [
                deformed[indices[0] as usize],
                deformed[indices[1] as usize],
                deformed[indices[2] as usize],
            ];
            let frame = deformed_surface_frame(points, vertices.map(|vertex| vertex.uv0))?;
            let barycentric = triangle_hit.barycentric;
            let uv = vertices[0].uv0 * barycentric[0]
                + vertices[1].uv0 * barycentric[1]
                + vertices[2].uv0 * barycentric[2];
            let rest_point = vertices[0].position * barycentric[0]
                + vertices[1].position * barycentric[1]
                + vertices[2].position * barycentric[2];
            let world_point = world_ray.origin + world_ray.dir * triangle_hit.distance;
            let tag =
                material_tag_for_triangle(&mesh_ref, triangle_index, material_tags.as_slice());
            let candidate = SceneSurfaceHit {
                entity,
                capabilities: SurfaceCapabilities {
                    ray: true,
                    project: true,
                    nearest: false,
                    uv: true,
                    authoritative_attachments: false,
                    authoritative_fields: false,
                },
                surface: SurfaceHit {
                    provider: provider_id,
                    position: WorldPosition::from_render_relative(
                        world_point,
                        WorldPosition::origin(),
                    )?,
                    distance_m,
                    frame,
                    coordinates: SurfaceCoordinates {
                        uv: Some(uv),
                        projection: rest_point.as_dvec3(),
                    },
                    attachment: None,
                    tags: tag
                        .map(|tag| {
                            vec![WeightedSurfaceTag {
                                tag,
                                weight: UnitInterval::ONE,
                            }]
                        })
                        .unwrap_or_default(),
                    revision,
                },
            };
            if scene_surface_is_nearer(&candidate, nearest.as_ref()) {
                nearest = Some(candidate);
            }
        }
    }
    Ok(nearest)
}

/// Lists every live mesh surface provider in stable provider-id order.
pub fn scene_surface_providers(
    gpu: &dyn GpuUploader,
    scene: &mut Scene,
    assets: &mut AssetServer,
) -> crate::Result<Vec<SceneSurfaceProvider>> {
    let mut skins = Vec::new();
    scene.for_each::<(&Transform, &SkinnedMesh), _>(|entity, (_, skin)| {
        skins.push((entity, skin.clone()));
    });
    let mut providers = static_scene_surface_snapshots(gpu, scene, assets)?
        .into_iter()
        .map(|(entity, provider)| SceneSurfaceProvider {
            entity,
            descriptor: provider.descriptor(),
        })
        .collect::<Vec<_>>();
    for (entity, skin) in skins {
        if scene.has_component::<PreviewGhost>(entity) {
            continue;
        }
        let Some(mesh) = assets.load_mesh_asset(gpu, skin.mesh) else {
            continue;
        };
        let Some(provider_id) = scene
            .component::<IdComponent>(entity)
            .ok()
            .map(|id| SurfaceProviderId(id.id.value()))
        else {
            continue;
        };
        let palette = scene.joint_matrices(&skin);
        if palette.is_empty() {
            continue;
        }
        let mut minimum = Vec3::splat(f32::MAX);
        let mut maximum = Vec3::splat(f32::MIN);
        for joint in &palette {
            world_aabb_from_corners(
                joint,
                mesh.bounds_min,
                mesh.bounds_max,
                &mut minimum,
                &mut maximum,
            );
        }
        let tags = surface_material_tags(scene, entity);
        providers.push(SceneSurfaceProvider {
            entity,
            descriptor: SurfaceProviderDescriptor {
                id: provider_id,
                revision: mesh_surface_revision(
                    skin.mesh.value(),
                    &mesh.cpu_vertices,
                    &mesh.cpu_indices,
                    &palette,
                    tags.as_slice(),
                ),
                bounds: WorldBounds::from_world_meters(minimum.as_dvec3(), maximum.as_dvec3())?,
                primitive_count: mesh.cpu_indices.len() as u64 / 3,
                max_tags_per_hit: u32::from(!tags.is_empty()),
                capabilities: SurfaceCapabilities {
                    ray: true,
                    project: true,
                    nearest: false,
                    uv: true,
                    authoritative_attachments: false,
                    authoritative_fields: false,
                },
            },
        });
    }
    providers.sort_by_key(|provider| provider.descriptor.id);
    Ok(providers)
}

/// Captures every authoritative static-mesh surface provider as an immutable worker-safe snapshot.
pub fn scene_surface_field_snapshots(
    gpu: &dyn GpuUploader,
    scene: &mut Scene,
    assets: &mut AssetServer,
) -> crate::Result<Vec<Arc<dyn SurfaceField>>> {
    Ok(static_scene_surface_snapshots(gpu, scene, assets)?
        .into_iter()
        .map(|(_, provider)| Arc::new(provider) as Arc<dyn SurfaceField>)
        .collect())
}

fn static_scene_surface_snapshots(
    gpu: &dyn GpuUploader,
    scene: &mut Scene,
    assets: &mut AssetServer,
) -> crate::Result<Vec<(Entity, StaticMeshSurfaceProvider)>> {
    let mut statics = Vec::new();
    scene.for_each::<(&Transform, &MeshComponent), _>(|entity, (_, mesh)| {
        statics.push((entity, *mesh));
    });
    let mut providers = Vec::new();
    for (entity, mesh) in statics {
        if scene.has_component::<PreviewGhost>(entity) {
            continue;
        }
        if let Some(provider) = static_mesh_surface_provider(gpu, scene, assets, entity, mesh)? {
            providers.push((entity, provider));
        }
    }
    providers.sort_by_key(|(_, provider)| provider.descriptor().id);
    Ok(providers)
}

/// Samples one static mesh provider's canonical scalar channel.
pub fn sample_scene_surface_field(
    gpu: &dyn GpuUploader,
    scene: &mut Scene,
    assets: &mut AssetServer,
    provider_id: SurfaceProviderId,
    channel: FieldChannel,
    derivative: FieldDerivative,
    position: WorldPosition,
) -> crate::Result<Option<FieldSample>> {
    let Some(entity) = scene.find_entity_by_uuid(saffron_core::Uuid(provider_id.0)) else {
        return Ok(None);
    };
    let Ok(mesh) = scene.component::<MeshComponent>(entity) else {
        return Ok(None);
    };
    let Some(provider) = static_mesh_surface_provider(gpu, scene, assets, entity, mesh)? else {
        return Ok(None);
    };
    provider
        .sample_scalar(channel, derivative, position)
        .map(Some)
        .map_err(Into::into)
}

fn static_mesh_surface_provider(
    gpu: &dyn GpuUploader,
    scene: &Scene,
    assets: &mut AssetServer,
    entity: Entity,
    mesh: MeshComponent,
) -> crate::Result<Option<StaticMeshSurfaceProvider>> {
    let Some(mesh_ref) = assets.load_mesh_asset(gpu, mesh.mesh) else {
        return Ok(None);
    };
    if mesh_ref.cpu_vertices.is_empty() {
        return Ok(None);
    }
    let Some(bvh) = assets.mesh_pick_bvh(mesh.mesh, &mesh_ref) else {
        return Ok(None);
    };
    let Some(provider_id) = scene
        .component::<IdComponent>(entity)
        .ok()
        .map(|id| SurfaceProviderId(id.id.value()))
    else {
        return Ok(None);
    };
    let model = scene.world_matrix(entity);
    let material_tags = surface_material_tags(scene, entity);
    let material_assets = assets.resolve_entity_material_assets(scene, entity, &mesh_ref.submeshes);
    let coverage = canonical_cpu_coverage(assets, &material_assets, gpu.coverage_temporal_phase());
    let revision = mesh_surface_revision(
        mesh.mesh.value(),
        &mesh_ref.cpu_vertices,
        &mesh_ref.cpu_indices,
        &[model],
        material_tags.as_slice(),
    );
    StaticMeshSurfaceProvider::new(StaticMeshSurfaceInput {
        id: provider_id,
        revision,
        vertices: Arc::clone(&mesh_ref.cpu_vertices),
        indices: Arc::clone(&mesh_ref.cpu_indices),
        submeshes: mesh_ref.submeshes.clone(),
        bounds_min: mesh_ref.bounds_min,
        bounds_max: mesh_ref.bounds_max,
        bvh,
        model,
        render_origin: WorldPosition::origin(),
        material_tags,
        coverage,
    })
    .map(Some)
    .map_err(Into::into)
}

fn canonical_cpu_coverage(
    assets: &mut AssetServer,
    materials: &[MaterialAsset],
    temporal_phase: u32,
) -> Vec<CanonicalCpuCoverage> {
    materials
        .iter()
        .map(|material| {
            let standard_masked = material.blend == "masked";
            let (
                source_kind,
                classification,
                source_id,
                hash_extent,
                salt,
                reference_cutoff,
                canonical_probability,
            ) = match &material.surface {
                MaterialSurface::Standard => (
                    CoverageSourceKind::AlbedoAlpha,
                    if standard_masked {
                        AlphaClassification::Masked
                    } else {
                        AlphaClassification::Opaque
                    },
                    material.albedo_texture,
                    [1, 1],
                    0,
                    material.alpha_cutoff,
                    false,
                ),
                MaterialSurface::ThinSheetFoliage(parameters) => {
                    let (kind, id) = match parameters.coverage_source {
                        CoverageSource::AlbedoAlpha => {
                            (CoverageSourceKind::AlbedoAlpha, material.albedo_texture)
                        }
                        CoverageSource::Texture(id) => (CoverageSourceKind::Texture, id),
                        CoverageSource::ModeledGeometry => {
                            (CoverageSourceKind::ModeledGeometry, saffron_core::Uuid(0))
                        }
                    };
                    (
                        kind,
                        parameters.coverage.classification,
                        id,
                        parameters.coverage.source_extent,
                        parameters.coverage.spatial_hash_salt,
                        parameters.coverage.reference_cutoff.to_f64() as f32,
                        true,
                    )
                }
            };
            let decoded = (source_id.value() != 0)
                .then(|| assets.load_texture_pixels(source_id))
                .flatten();
            let (texture_extent, alpha) = decoded.map_or(([1, 1], None), |decoded| {
                let rgba = if canonical_probability {
                    crate::coverage_preserving_mips(
                        &decoded.rgba,
                        decoded.width,
                        decoded.height,
                        match &material.surface {
                            MaterialSurface::ThinSheetFoliage(parameters) => {
                                parameters.coverage.reference_cutoff.bits()
                            }
                            MaterialSurface::Standard => 0,
                        },
                    )
                    .into_iter()
                    .next()
                    .map_or_else(|| decoded.rgba.clone(), |mip| mip.rgba)
                } else {
                    decoded.rgba.clone()
                };
                let alpha = rgba
                    .chunks_exact(4)
                    .map(|pixel| pixel[3])
                    .collect::<Vec<_>>()
                    .into();
                ([decoded.width, decoded.height], Some(alpha))
            });
            CanonicalCpuCoverage {
                source_kind,
                classification,
                base_color_alpha: material.base_color.w,
                texture_extent,
                hash_extent,
                salt,
                temporal_phase,
                reference_cutoff,
                canonical_probability,
                uv_tiling: material.uv_tiling,
                uv_offset: material.uv_offset,
                alpha,
            }
        })
        .collect()
}

fn surface_material_tags(scene: &Scene, entity: Entity) -> Vec<SurfaceTagId> {
    let tags: Vec<SurfaceTagId> = scene
        .with_component::<MaterialSet, _>(entity, |materials| {
            materials
                .slots
                .iter()
                .map(|slot| SurfaceTagId(slot.material.value()))
                .collect()
        })
        .unwrap_or_default();
    if tags.is_empty() {
        vec![SurfaceTagId(crate::DEFAULT_MATERIAL_ID.value())]
    } else {
        tags
    }
}

fn material_tag_for_triangle(
    mesh: &GpuMesh,
    triangle_index: u32,
    tags: &[SurfaceTagId],
) -> Option<SurfaceTagId> {
    let index_offset = triangle_index.saturating_mul(3);
    mesh.submeshes.iter().find_map(|submesh| {
        let end = submesh.first_index.saturating_add(submesh.index_count);
        (index_offset >= submesh.first_index && index_offset < end)
            .then(|| tags.get(submesh.material_slot as usize).copied())
            .flatten()
    })
}

fn mesh_surface_revision(
    mesh_id: u64,
    vertices: &[Vertex],
    indices: &[u32],
    transforms: &[Mat4],
    tags: &[SurfaceTagId],
) -> SurfaceRevision {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    let mut fold = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    };
    fold(&mesh_id.to_le_bytes());
    for vertex in vertices {
        for component in vertex.position.to_array() {
            fold(&component.to_bits().to_le_bytes());
        }
        for component in vertex.normal.to_array() {
            fold(&component.to_bits().to_le_bytes());
        }
        for component in vertex.uv0.to_array() {
            fold(&component.to_bits().to_le_bytes());
        }
        for component in vertex.tangent {
            fold(&component.to_bits().to_le_bytes());
        }
    }
    for index in indices {
        fold(&index.to_le_bytes());
    }
    for transform in transforms {
        for component in transform.to_cols_array() {
            fold(&component.to_bits().to_le_bytes());
        }
    }
    for tag in tags {
        fold(&tag.0.to_le_bytes());
    }
    SurfaceRevision(hash)
}

fn deformed_surface_frame(points: [Vec3; 3], uv: [Vec2; 3]) -> crate::Result<SurfaceFrame> {
    let edge1 = points[1] - points[0];
    let edge2 = points[2] - points[0];
    let normal = edge1.cross(edge2).normalize_or_zero();
    let duv1 = uv[1] - uv[0];
    let duv2 = uv[2] - uv[0];
    let determinant = duv1.x * duv2.y - duv1.y * duv2.x;
    if determinant.abs() <= 1e-12 {
        return SurfaceFrame::from_normal(normal).map_err(Into::into);
    }
    let inverse = 1.0 / determinant;
    let tangent = (edge1 * duv2.y - edge2 * duv1.y) * inverse;
    let bitangent = (edge2 * duv1.x - edge1 * duv2.x) * inverse;
    let handedness = if normal.cross(tangent).dot(bitangent) < 0.0 {
        -1.0
    } else {
        1.0
    };
    SurfaceFrame::new(normal, tangent, handedness).map_err(Into::into)
}

fn scene_surface_is_nearer(candidate: &SceneSurfaceHit, current: Option<&SceneSurfaceHit>) -> bool {
    current.is_none_or(|current| {
        candidate.surface.distance_m < current.surface.distance_m
            || (candidate.surface.distance_m == current.surface.distance_m
                && candidate.surface.provider < current.surface.provider)
    })
}

/// Builds the world-space viewport ray used by picking and placement.
/// The viewport pick ray in the vegetation query vocabulary — the same origin and
/// direction the scene-surface pick casts, with a finite far bound.
#[must_use]
pub fn viewport_pick_ray(
    viewport: (u32, u32),
    camera: &CameraView,
    ndc: Vec2,
) -> Option<saffron_vegetation::VegetationQueryRay> {
    if viewport.0 == 0 || viewport.1 == 0 {
        return None;
    }
    let ray = viewport_ray(viewport, camera, ndc);
    let origin = WorldPosition::from_render_relative(ray.origin, WorldPosition::origin()).ok()?;
    saffron_vegetation::VegetationQueryRay::new(origin, ray.dir.as_dvec3(), 10_000.0).ok()
}

pub fn viewport_ray(viewport: (u32, u32), camera: &CameraView, ndc: Vec2) -> Ray {
    let (width, height) = viewport;
    let aspect = width as f32 / height as f32;
    let mut proj = camera_projection(camera, aspect);
    proj.y_axis.y *= -1.0;
    let inv_view_proj = (proj * camera.view).inverse();
    let near_h = inv_view_proj * Vec4::new(ndc.x, ndc.y, 0.0, 1.0);
    let far_h = inv_view_proj * Vec4::new(ndc.x, ndc.y, 1.0, 1.0);
    let origin = near_h.truncate() / near_h.w;
    Ray {
        origin,
        dir: (far_h.truncate() / far_h.w - origin).normalize(),
    }
}

/// Walks a deformed triangle soup and reports the nearest source triangle with barycentrics.
fn nearest_triangle_filtered(
    ray: &Ray,
    positions: &[Vec3],
    indices: &[u32],
    mut filter: impl FnMut(u32, &saffron_geometry::TriangleRayHit) -> bool,
) -> Option<(u32, saffron_geometry::TriangleRayHit)> {
    let mut best: Option<(u32, saffron_geometry::TriangleRayHit)> = None;
    for (triangle_index, tri) in indices.chunks_exact(3).enumerate() {
        let (a, b, c) = (
            positions[tri[0] as usize],
            positions[tri[1] as usize],
            positions[tri[2] as usize],
        );
        if let Some(hit) = ray_triangle_coordinates(ray, a, b, c) {
            let triangle_index = triangle_index as u32;
            if !filter(triangle_index, &hit) {
                continue;
            }
            let replace = best.is_none_or(|(current_index, current)| {
                hit.distance < current.distance
                    || (hit.distance == current.distance && triangle_index < current_index)
            });
            if replace {
                best = Some((triangle_index, hit));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn viewport_ray_maps_upper_screen_to_world_up() {
        // The pick convention (viewport UV 0,0 = top-left → y-down NDC): the projection's
        // y is flipped, so an upper-screen point (ndc.y < 0) casts a ray pointing toward world
        // +Y and a lower-screen point (ndc.y > 0) toward world -Y. Guards the double y-flip
        // regression where clicking above an object hit below it (the ray mirrored about center).
        let camera = test_camera(); // eye at +Z, looking down -Z, up +Y.
        let viewport = (1024, 768);
        let up = viewport_ray(viewport, &camera, Vec2::new(0.0, -0.5));
        let center = viewport_ray(viewport, &camera, Vec2::ZERO);
        let down = viewport_ray(viewport, &camera, Vec2::new(0.0, 0.5));
        assert!(up.dir.y > 0.0, "upper screen (ndc.y<0) aims at world +Y");
        assert!(down.dir.y < 0.0, "lower screen (ndc.y>0) aims at world -Y");
        assert!(center.dir.y.abs() < 1e-4, "screen center aims level");
        // The mirrored point about center is the exact opposite tilt, never the same hemisphere.
        assert!(
            up.dir.y > 0.0 && down.dir.y < 0.0 && (up.dir.y + down.dir.y).abs() < 1e-4,
            "the ray is not mirrored about screen center"
        );
    }

    #[test]
    fn pick_hits_a_mesh_through_its_center_and_misses_empty_space() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let (mut assets, tmp) = scratch_server("pick");
        write_triangle_mesh(&mut assets, saffron_core::Uuid(5100), "tri");

        let mut scene = Scene::new();
        let e = scene.create_entity("Tri");
        scene
            .add_component(
                e,
                MeshComponent {
                    mesh: saffron_core::Uuid(5100),
                },
            )
            .unwrap();
        // Flatten the hierarchy so the world matrix the pick reads is current.
        scene.update_world_transforms();

        let renderer =
            RecordingRenderer::new(1024, 768, false).with_gpu(&fx.uploader, &fx.descriptors);
        let camera = test_camera();
        // The triangle straddles the origin; a ray through clip-space center (0,0) hits it.
        let hit = pick_entity(
            &renderer,
            (1024, 768),
            &mut scene,
            &mut assets,
            &camera,
            Vec2::ZERO,
        )
        .unwrap();
        assert_eq!(hit, e, "a click through the center hits the triangle");

        // A click far in the corner of the loose AABB but outside the triangle misses (the
        // narrow-phase ray-triangle rejects the empty corner).
        let miss = pick_entity(
            &renderer,
            (1024, 768),
            &mut scene,
            &mut assets,
            &camera,
            Vec2::new(-0.99, -0.99),
        )
        .unwrap();
        assert_eq!(miss, Entity::NULL, "a click into empty space misses");

        drop(renderer);
        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pick_resolves_a_skinned_mesh_against_a_fresh_joint_palette() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let (mut assets, tmp) = scratch_server("pick-skin");
        write_skinned_triangle(&mut assets, saffron_core::Uuid(5400), "rig");

        let mut scene = Scene::new();
        let e = spawn_one_bone_skin(&mut scene, saffron_core::Uuid(5400));
        scene.update_world_transforms();

        let renderer =
            RecordingRenderer::new(1024, 768, false).with_gpu(&fx.uploader, &fx.descriptors);
        let camera = test_camera();
        // The bone is at the origin with identity inverse-bind, so the rest triangle straddles
        // the origin; a center ray skins each vertex through the (identity) palette and hits.
        let hit = pick_entity(
            &renderer,
            (1024, 768),
            &mut scene,
            &mut assets,
            &camera,
            Vec2::ZERO,
        )
        .unwrap();
        assert_eq!(hit, e, "the skinned triangle picks against a fresh palette");

        drop(renderer);
        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
