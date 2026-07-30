use saffron_assets::{sample_scene_surface_field, scene_surface_providers};
use saffron_geometry::glam::{Vec2, Vec3 as GlamVec3};
use saffron_protocol::{
    EmptyParams, EntityParams, EntityRef, PickKind, PickParams, PickResult, ResidencyCountsDto,
    ResidencyFacetDto, SpatialBoundsDto, SpatialCellParams, SpatialCellResult,
    SpatialFieldChannelDto, SpatialFieldDerivativeDto, SpatialLocalPositionDto,
    SpatialResidencyCellDto, SpatialResidencyResult, SpatialSampleParams, SpatialSampleResult,
    SpatialSourceDto, SpatialSourceLevelDto, SpatialTicksDto, SpatialWorldPositionDto,
    SurfaceCapabilitiesDto, SurfaceProviderDto, SurfaceProvidersResult, Uuid as WireUuid, Vec3,
    WorldCellKeyDto,
};
use saffron_scene::{Camera, CameraView, Entity, Mesh, PointLight, SpotLight, Transform};
use saffron_sceneedit::{SceneEditContext, viewport_project};
use saffron_spatial::{
    FieldChannel, FieldDerivative, ResidencyFacet, SurfaceCapabilities, WorldCellKey, WorldPosition,
};

use crate::error::Error;
use crate::registry::CommandRegistry;
use crate::selector::{entity_ref_dto, resolve_entity};

/// Converts a wire `Vec3` to glam.
pub(crate) fn to_glam3(v: Vec3) -> GlamVec3 {
    GlamVec3::new(v.x, v.y, v.z)
}

/// Converts a glam vector to the wire `Vec3`.
pub(crate) fn from_glam3(v: GlamVec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

pub(crate) fn spatial_ticks_dto(ticks: [i128; 3]) -> SpatialTicksDto {
    SpatialTicksDto {
        x: ticks[0].to_string(),
        y: ticks[1].to_string(),
        z: ticks[2].to_string(),
    }
}

pub(crate) fn world_cell_dto(cell: WorldCellKey) -> WorldCellKeyDto {
    let coordinates = cell.coordinates();
    WorldCellKeyDto {
        x: coordinates[0].to_string(),
        y: coordinates[1].to_string(),
        z: coordinates[2].to_string(),
        level: cell.level(),
        canonical_hex: cell
            .canonical_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

pub(crate) fn world_position_dto(position: WorldPosition) -> SpatialWorldPositionDto {
    let local = position.local().ticks();
    SpatialWorldPositionDto {
        cell: world_cell_dto(position.cell()),
        local: SpatialLocalPositionDto {
            x: local[0],
            y: local[1],
            z: local[2],
        },
        global_ticks: spatial_ticks_dto(position.global_ticks()),
    }
}

pub(crate) fn surface_capabilities_dto(
    capabilities: SurfaceCapabilities,
) -> SurfaceCapabilitiesDto {
    SurfaceCapabilitiesDto {
        ray: capabilities.ray,
        project: capabilities.project,
        nearest: capabilities.nearest,
        uv: capabilities.uv,
        authoritative_attachments: capabilities.authoritative_attachments,
        authoritative_fields: capabilities.authoritative_fields,
    }
}

pub(crate) fn field_channel(
    channel: SpatialFieldChannelDto,
    user_channel: Option<&str>,
) -> crate::Result<FieldChannel> {
    let ordinary = match channel {
        SpatialFieldChannelDto::Altitude => Some(FieldChannel::Altitude),
        SpatialFieldChannelDto::Slope => Some(FieldChannel::Slope),
        SpatialFieldChannelDto::Curvature => Some(FieldChannel::Curvature),
        SpatialFieldChannelDto::Concavity => Some(FieldChannel::Concavity),
        SpatialFieldChannelDto::Drainage => Some(FieldChannel::Drainage),
        SpatialFieldChannelDto::Moisture => Some(FieldChannel::Moisture),
        SpatialFieldChannelDto::Temperature => Some(FieldChannel::Temperature),
        SpatialFieldChannelDto::Precipitation => Some(FieldChannel::Precipitation),
        SpatialFieldChannelDto::Sunlight => Some(FieldChannel::Sunlight),
        SpatialFieldChannelDto::Exposure => Some(FieldChannel::Exposure),
        SpatialFieldChannelDto::WaterDistance => Some(FieldChannel::WaterDistance),
        SpatialFieldChannelDto::WaterDepth => Some(FieldChannel::WaterDepth),
        SpatialFieldChannelDto::SignedBlocker => Some(FieldChannel::SignedBlocker),
        SpatialFieldChannelDto::SplineDistance => Some(FieldChannel::SplineDistance),
        SpatialFieldChannelDto::User => None,
    };
    match (ordinary, user_channel) {
        (Some(channel), None) => Ok(channel),
        (Some(_), Some(_)) => Err(Error::command(
            "userChannel is valid only when channel is 'user'",
        )),
        (None, Some(value)) => value
            .parse::<u64>()
            .map(FieldChannel::User)
            .map_err(|_| Error::command("userChannel must be a decimal u64")),
        (None, None) => Err(Error::command(
            "userChannel is required when channel is 'user'",
        )),
    }
}

pub(crate) fn field_derivative(derivative: SpatialFieldDerivativeDto) -> FieldDerivative {
    match derivative {
        SpatialFieldDerivativeDto::Value => FieldDerivative::Value,
        SpatialFieldDerivativeDto::Gradient => FieldDerivative::Gradient,
        SpatialFieldDerivativeDto::Hessian => FieldDerivative::Hessian,
    }
}

pub(crate) fn residency_facet_dto(facet: ResidencyFacet) -> ResidencyFacetDto {
    match facet {
        ResidencyFacet::Render => ResidencyFacetDto::Render,
        ResidencyFacet::Physics => ResidencyFacetDto::Physics,
        ResidencyFacet::Simulation => ResidencyFacetDto::Simulation,
        ResidencyFacet::Editing => ResidencyFacetDto::Editing,
        ResidencyFacet::Navigation => ResidencyFacetDto::Navigation,
        ResidencyFacet::Network => ResidencyFacetDto::Network,
    }
}

/// Server-side billboard hit-test: the nearest meshless light/camera entity whose
/// screen-space glyph contains `mouse` (viewport pixels).
pub(crate) fn pick_billboard(
    ctx: &mut SceneEditContext,
    cam: &CameraView,
    width: u32,
    height: u32,
    mouse: Vec2,
) -> Entity {
    if width == 0 || height == 0 {
        return Entity::NULL;
    }
    // A touch larger than the drawn glyph for easier clicking.
    const HALF: f32 = 13.0;
    let scene = ctx.active_scene();

    // Collect candidate entities first (a `for_each` borrows the scene mutably), then
    // hit-test each — the light/camera billboard set: a meshless entity that is a point
    // light, or a spot light that is not also a point light, or a camera that is neither.
    let mut candidates: Vec<Entity> = Vec::new();
    scene.for_each::<&PointLight, _>(|e, _| candidates.push(e));
    scene.for_each::<&SpotLight, _>(|e, _| candidates.push(e));
    scene.for_each::<&Camera, _>(|e, _| candidates.push(e));

    let mut hit = Entity::NULL;
    let mut best = HALF;
    for e in candidates {
        if !scene.has_component::<Transform>(e) || scene.has_component::<Mesh>(e) {
            continue;
        }
        // De-dupe: a point light is tested via the PointLight pass; a spot light only
        // when not also a point light; a camera only when neither.
        let is_point = scene.has_component::<PointLight>(e);
        let is_spot = scene.has_component::<SpotLight>(e);
        let pos = scene.world_translation(e);
        let p = viewport_project(cam, width, height, pos);
        if !p.visible {
            continue;
        }
        let _ = (is_point, is_spot); // the candidate set already encodes the precedence
        let d = (mouse - p.pixel).abs();
        if d.x <= HALF && d.y <= HALF {
            let dist = (mouse - p.pixel).length();
            if dist <= best {
                best = dist;
                hit = e;
            }
        }
    }
    hit
}

/// Registers selection picking and the spatial query commands.
pub(crate) fn register_picking(reg: &mut CommandRegistry) {
    reg.register::<EntityParams, EntityRef>("select", "select {entity}", |ctx, params| {
        let entity = resolve_entity(ctx, &params.entity)?;
        ctx.scene_edit.set_selection(entity);
        let scene = ctx.scene_edit.active_scene();
        Ok(entity_ref_dto(scene, entity))
    });

    reg.register::<PickParams, PickResult>(
        "pick",
        "pick {u=0.5, v=0.5} — pick at viewport UV (0,0 = top-left); tests billboards then mesh AABBs",
        |ctx, params| {
            let u = params.u.unwrap_or(0.5);
            let v = params.v.unwrap_or(0.5);
            // The eye the frame was rendered with, so a click during play ray-casts from the
            // game camera, not the parked fly-cam.
            let cam = ctx.scene_edit.render_camera_view();
            let width = ctx.renderer.viewport_width();
            let height = ctx.renderer.viewport_height();
            let mouse = Vec2::new(u * width as f32, v * height as f32);

            // Billboards first (light/camera glyphs aren't in the mesh AABB set), then the
            // mesh ray-pick. The glyph hit rect mirrors the overlay's ~12px half-size.
            let billboard = pick_billboard(ctx.scene_edit, &cam, width, height, mouse);
            if billboard != Entity::NULL {
                ctx.scene_edit.set_selection(billboard);
                let scene = ctx.scene_edit.active_scene();
                let r = entity_ref_dto(scene, billboard);
                return Ok(PickResult {
                    hit: true,
                    id: Some(r.id),
                    name: Some(r.name),
                    kind: Some(PickKind::Billboard),
                    plant: None,
                    position: None,
                    normal: None,
                });
            }

            // pick_scene_surface flips proj[1][1] to match the renderer's clip space, so it
            // expects y-down NDC: v=0 (viewport top) maps to ndc.y=-1.
            let ndc = Vec2::new(u * 2.0 - 1.0, v * 2.0 - 1.0);
            let assets = &mut *ctx.assets;
            let viewport = (width, height);
            let mut hit_result = Ok(None);
            // The borrow split: the surface pick needs the upload seam + the active scene +
            // the asset server at once. The scene is borrowed from scene_edit; take it inside
            // the upload closure so the renderer borrow does not overlap it.
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                hit_result = saffron_assets::pick_scene_surface(
                    gpu,
                    viewport,
                    ctx.scene_edit.active_scene(),
                    assets,
                    &cam,
                    ndc,
                );
            });
            let surface_hit = hit_result.map_err(Error::command)?;
            // The same viewport ray tests the resident macro vegetation; the nearest of the
            // two vocabularies wins. Plants resolve through the CPU cell snapshot to their
            // stable identity — never a GPU slot.
            let pick_ray = saffron_assets::viewport_pick_ray(viewport, &cam, ndc);
            let plant_hit = ctx
                .vegetation
                .as_ref()
                .zip(pick_ray)
                .and_then(|(world, ray)| {
                    world
                        .query_ray(ray, &saffron_vegetation::VegetationQueryFilter::default())
                        .ok()?
                        .into_iter()
                        .next()
                })
                .filter(|plant| {
                    surface_hit
                        .as_ref()
                        .is_none_or(|hit| plant.distance_m < hit.surface.distance_m)
                });
            if let Some(nearest) = plant_hit {
                ctx.scene_edit.set_selection(Entity::NULL);
                return Ok(PickResult {
                    hit: true,
                    id: None,
                    name: None,
                    kind: Some(PickKind::Vegetation),
                    plant: Some(saffron_protocol::PlantId(nearest.plant.plant.to_string())),
                    position: None,
                    normal: None,
                });
            }
            // A micro-field ground hit is nonpersistent paint feedback: it never beats
            // an entity surface or a macro plant, and it carries no identity.
            let micro_hit = ctx
                .vegetation
                .as_ref()
                .zip(pick_ray)
                .and_then(|(world, ray)| world.query_micro_ray(ray))
                .filter(|micro| {
                    surface_hit
                        .as_ref()
                        .is_none_or(|hit| micro.distance_m < hit.surface.distance_m)
                });
            if let Some(micro) = micro_hit {
                ctx.scene_edit.set_selection(Entity::NULL);
                return Ok(PickResult {
                    hit: true,
                    id: None,
                    name: None,
                    kind: Some(PickKind::MicroVegetation),
                    plant: None,
                    position: Some(micro.position.to_array()),
                    normal: None,
                });
            }
            let Some(surface) = surface_hit else {
                ctx.scene_edit.set_selection(Entity::NULL);
                return Ok(PickResult {
                    hit: false,
                    id: None,
                    name: None,
                    kind: None,
                    plant: None,
                    position: None,
                    normal: None,
                });
            };
            let hit = surface.entity;
            // A model instance is a single subtree; a click anywhere in it selects the whole
            // model (its container root), not the bare mesh/bone node the ray hit.
            let selected = ctx.scene_edit.active_scene().model_root_of(hit);
            ctx.scene_edit.set_selection(selected);
            let scene = ctx.scene_edit.active_scene();
            let r = entity_ref_dto(scene, selected);
            Ok(PickResult {
                hit: true,
                id: Some(r.id),
                name: Some(r.name),
                kind: Some(PickKind::Mesh),
                plant: None,
                position: Some(surface.surface.position.world_meters().to_array()),
                normal: Some(surface.surface.frame.normal.to_array()),
            })
        },
    );

    reg.register::<saffron_protocol::QuerySurfaceRayParams, saffron_protocol::SurfaceRayResult>(
        "query-surface-ray",
        "query-surface-ray {originM, direction, maxDistanceM?} — nearest scene-surface hit",
        |ctx, params| {
            let origin = saffron_spatial::WorldPosition::from_global_ticks(
                params
                    .origin_m
                    .map(|meters| (meters * 4096.0).round() as i128),
            )
            .map_err(Error::command)?;
            let direction = saffron_geometry::glam::DVec3::new(
                f64::from(params.direction[0]),
                f64::from(params.direction[1]),
                f64::from(params.direction[2]),
            );
            let ray = saffron_spatial::SurfaceRay::new(
                origin,
                direction.normalize_or_zero(),
                params.max_distance_m.unwrap_or(10_000.0),
            )
            .map_err(Error::command)?;
            let assets = &mut *ctx.assets;
            let mut hit_result = Ok(None);
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                hit_result = saffron_assets::query_scene_surface_ray(
                    gpu,
                    ctx.scene_edit.active_scene(),
                    assets,
                    &ray,
                );
            });
            let hit = hit_result.map_err(Error::command)?;
            Ok(match hit {
                Some(surface) => saffron_protocol::SurfaceRayResult {
                    hit: true,
                    position: Some(surface.surface.position.world_meters().to_array()),
                    normal: Some(surface.surface.frame.normal.to_array()),
                },
                None => saffron_protocol::SurfaceRayResult {
                    hit: false,
                    position: None,
                    normal: None,
                },
            })
        },
    );

    reg.register::<SpatialCellParams, SpatialCellResult>(
        "spatial-cell",
        "spatial-cell {world? | ticks?, level?} — canonical position and owner cell",
        |_ctx, params| {
            if params.world.is_some() && params.ticks.is_some() {
                return Err(Error::command("provide world or ticks, not both"));
            }
            let position = if let Some(ticks) = params.ticks {
                let parse = |value: &str| {
                    value
                        .parse::<i128>()
                        .map_err(|_| Error::command("ticks must be signed decimal integers"))
                };
                WorldPosition::from_global_ticks([
                    parse(&ticks.x)?,
                    parse(&ticks.y)?,
                    parse(&ticks.z)?,
                ])
                .map_err(Error::command)?
            } else {
                let world = params.world.unwrap_or(Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                });
                WorldPosition::from_world_meters(saffron_geometry::glam::DVec3::new(
                    f64::from(world.x),
                    f64::from(world.y),
                    f64::from(world.z),
                ))
                .map_err(Error::command)?
            };
            let selected_cell = position
                .cell()
                .ancestor(params.level.unwrap_or(0))
                .map_err(Error::command)?;
            Ok(SpatialCellResult {
                position: world_position_dto(position),
                selected_cell: world_cell_dto(selected_cell),
            })
        },
    );

    reg.register::<EmptyParams, SurfaceProvidersResult>(
        "spatial-providers",
        "spatial-providers — list live surface providers and capabilities",
        |ctx, _params| {
            let assets = &mut *ctx.assets;
            let mut result = Ok(Vec::new());
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = scene_surface_providers(gpu, ctx.scene_edit.active_scene(), assets);
            });
            let providers = result
                .map_err(Error::command)?
                .into_iter()
                .map(|provider| {
                    let descriptor = provider.descriptor;
                    let (entity, name) = {
                        let scene = ctx.scene_edit.active_scene();
                        let reference = entity_ref_dto(scene, provider.entity);
                        (reference.id, reference.name)
                    };
                    SurfaceProviderDto {
                        id: WireUuid(descriptor.id.0),
                        entity,
                        name,
                        revision: descriptor.revision.0.to_string(),
                        bounds: SpatialBoundsDto {
                            min_ticks: spatial_ticks_dto(descriptor.bounds.min_ticks()),
                            max_ticks_exclusive: spatial_ticks_dto(
                                descriptor.bounds.max_ticks_exclusive(),
                            ),
                        },
                        primitive_count: descriptor.primitive_count.to_string(),
                        max_tags_per_hit: descriptor.max_tags_per_hit,
                        capabilities: surface_capabilities_dto(descriptor.capabilities),
                    }
                })
                .collect();
            Ok(SurfaceProvidersResult { providers })
        },
    );

    reg.register::<SpatialSampleParams, SpatialSampleResult>(
        "spatial-sample",
        "spatial-sample {provider, channel, position, derivative?}",
        |ctx, params| {
            let derivative_dto = params.derivative.unwrap_or_default();
            let channel = field_channel(params.channel, params.user_channel.as_deref())?;
            let derivative = field_derivative(derivative_dto);
            let position = WorldPosition::from_world_meters(saffron_geometry::glam::DVec3::new(
                f64::from(params.position.x),
                f64::from(params.position.y),
                f64::from(params.position.z),
            ))
            .map_err(Error::command)?;
            let provider_id = saffron_spatial::SurfaceProviderId(params.provider.0);
            let assets = &mut *ctx.assets;
            let mut result = Ok(None);
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = sample_scene_surface_field(
                    gpu,
                    ctx.scene_edit.active_scene(),
                    assets,
                    provider_id,
                    channel,
                    derivative,
                    position,
                );
            });
            let sample = result
                .map_err(Error::command)?
                .ok_or_else(|| Error::command("surface provider not found"))?;
            Ok(SpatialSampleResult {
                provider: params.provider,
                channel: params.channel,
                user_channel: params.user_channel,
                derivative: derivative_dto,
                value_bits: sample.value.bits(),
                value: sample.value.to_f64(),
                revision: sample.revision.0.to_string(),
            })
        },
    );

    reg.register::<EmptyParams, SpatialResidencyResult>(
        "spatial-residency",
        "spatial-residency — list spatial sources and per-facet cell references",
        |ctx, _params| {
            let sources = ctx
                .spatial
                .sources()
                .into_iter()
                .map(|source| SpatialSourceDto {
                    id: source.id.0.to_string(),
                    revision: source.revision.to_string(),
                    position: world_position_dto(source.position),
                    velocity_mps: Vec3 {
                        x: source.velocity_mps.x as f32,
                        y: source.velocity_mps.y as f32,
                        z: source.velocity_mps.z as f32,
                    },
                    prediction_seconds: source.prediction_seconds,
                    levels: source
                        .levels
                        .into_iter()
                        .map(|level| SpatialSourceLevelDto {
                            level: level.level,
                            load_radius_cells: level.load_radius_cells,
                            cleanup_radius_cells: level.cleanup_radius_cells,
                        })
                        .collect(),
                    facets: source.facets.iter().map(residency_facet_dto).collect(),
                    priority: source.priority,
                })
                .collect();
            let cells = ctx
                .spatial
                .snapshots()
                .map_err(Error::command)?
                .into_iter()
                .map(|snapshot| SpatialResidencyCellDto {
                    cell: world_cell_dto(snapshot.cell),
                    reference_counts: ResidencyCountsDto {
                        render: snapshot.reference_counts[ResidencyFacet::Render as usize],
                        physics: snapshot.reference_counts[ResidencyFacet::Physics as usize],
                        simulation: snapshot.reference_counts[ResidencyFacet::Simulation as usize],
                        editing: snapshot.reference_counts[ResidencyFacet::Editing as usize],
                        navigation: snapshot.reference_counts[ResidencyFacet::Navigation as usize],
                        network: snapshot.reference_counts[ResidencyFacet::Network as usize],
                    },
                    priority: snapshot.priority,
                })
                .collect();
            Ok(SpatialResidencyResult { sources, cells })
        },
    );
}
