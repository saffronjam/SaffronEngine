use std::collections::BTreeMap;

use saffron_assets::{
    PlantRecookOptions, PlantRecookOutcome, load_plant_family_asset,
    portable_vegetation_platform_profile, recook_plant_family, validate_plant_family_sources,
    vegetation_cook_versions,
};
use saffron_protocol::{
    ControlDiagnosticDto, PlantRecookParams, PlantRecookResult, PlantValidateParams,
    PlantValidationResult, ReimportConflictDiagnosticDto, Uuid as WireUuid,
};
use saffron_vegetation::{PlantCompileDiagnosticSeverity, PlantCompileLimits};

use super::*;
use crate::error::{Error, Result};
use crate::registry::CommandRegistry;

pub fn register_plant_commands(reg: &mut CommandRegistry) {
    reg.register::<saffron_protocol::PlantCreateParams, saffron_protocol::PlantCreateResult>(
        "plant-create",
        "create a native plant family from the starter botanical graph",
        |ctx, params| {
            require_project_loaded(ctx)?;
            if params.name.trim().is_empty() {
                return Err(Error::command("plant name must not be empty"));
            }
            // A zero seed derives one from the name, so two plants created the same way are two
            // different individuals rather than the same tree twice.
            let seed = if params.seed.is_empty() || params.seed == "0" {
                saffron_vegetation::ContentHash::of(params.name.as_bytes()).bytes()[..16]
                    .try_into()
                    .map(u128::from_be_bytes)
                    .unwrap_or(1)
            } else {
                params
                    .seed
                    .parse::<u128>()
                    .map_err(|_| Error::command("seed must be a decimal u128"))?
            };
            // Wire UUIDs are catalog identities already; a material that is not in the catalog is
            // a caller mistake rather than something to invent a slot for.
            let materials = params
                .materials
                .iter()
                .map(|material| {
                    let id = saffron_core::Uuid(material.0);
                    ctx.assets
                        .catalog()
                        .entries
                        .iter()
                        .any(|entry| entry.id == id)
                        .then_some(id)
                        .ok_or_else(|| {
                            Error::command(format!("no material asset '{}'", material.0))
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            let graph = saffron_vegetation::BotanicalGraphDocument::sapling(seed);
            let family = saffron_vegetation::native_plant_family(
                saffron_core::Uuid::new(),
                params.name.trim(),
                graph,
                materials,
                &saffron_vegetation::NoBotanicalModules,
            )
            .map_err(Error::command)?;
            let growth = plant_growth_dto(
                ctx.assets,
                &family,
                0,
                &saffron_vegetation::BotanicalBudget::COOK,
            )?;
            let folder = if params.folder.trim().is_empty() {
                "plants"
            } else {
                params.folder.trim()
            };
            let plant = saffron_assets::save_plant_family_asset(
                ctx.assets,
                family,
                params.name.trim(),
                folder,
            )
            .map_err(Error::command)?;
            Ok(saffron_protocol::PlantCreateResult {
                plant: WireUuid(plant.value()),
                growth,
            })
        },
    );
    reg.register::<saffron_protocol::PlantGrowthParams, saffron_protocol::PlantGraphResult>(
        "plant-graph",
        "read one native plant family's botanical graph and what it grows",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
            let saffron_vegetation::PlantFamilySource::Native { graph, grafts } = &plant.source
            else {
                return Err(Error::command(
                    "plant family has an imported source, not a botanical graph",
                ));
            };
            Ok(saffron_protocol::PlantGraphResult {
                plant: WireUuid(id.value()),
                graph: crate::botanical_dto::graph_dto(graph),
                grafts: grafts
                    .iter()
                    .map(crate::botanical_dto::graft_source_dto)
                    .collect(),
                modules: plant
                    .modules
                    .iter()
                    .map(crate::botanical_dto::module_reference_dto)
                    .collect(),
                growth: plant_growth_dto(
                    ctx.assets,
                    &plant,
                    params.variation,
                    &preview_budget(params.max_axes, params.max_elements),
                )?,
            })
        },
    );
    reg.register::<saffron_protocol::PlantGraphSetParams, saffron_protocol::PlantGraphResult>(
        "plant-graph-set",
        "replace one native plant family's botanical graph and regrow it",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let existing = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
            if !matches!(
                existing.source,
                saffron_vegetation::PlantFamilySource::Native { .. }
            ) {
                return Err(Error::command(
                    "plant family has an imported source, not a botanical graph",
                ));
            }
            let graph = crate::botanical_dto::graph_from_dto(&params.graph)?;
            // A graft source keeps whatever content hash the last cook observed; a newly declared
            // one starts empty and the next cook records what it read.
            let previous: std::collections::BTreeMap<u128, [u8; 32]> = match &existing.source {
                saffron_vegetation::PlantFamilySource::Native { grafts, .. } => grafts
                    .iter()
                    .map(|graft| (graft.id, graft.content_hash))
                    .collect(),
                saffron_vegetation::PlantFamilySource::Imported(_) => Default::default(),
            };
            let mut grafts = params
                .grafts
                .iter()
                .map(|graft| {
                    let mut resolved = crate::botanical_dto::graft_source_from_dto(graft)?;
                    if let Some(hash) = previous.get(&resolved.id) {
                        resolved.content_hash = *hash;
                    }
                    Ok(resolved)
                })
                .collect::<Result<Vec<_>>>()?;
            grafts.sort_by_key(|graft| graft.id);
            // A `moduleCall` node and the binding it names are validated against each other, so the
            // module table crosses with the graph rather than leaving one of the two behind.
            let mut modules = params
                .modules
                .iter()
                .map(crate::botanical_dto::module_reference_from_dto)
                .collect::<Result<Vec<_>>>()?;
            modules.sort_by_key(|module| module.call_guid);
            let with_modules = saffron_vegetation::PlantFamilyAsset {
                modules,
                ..existing.clone()
            };
            let module_resolver =
                saffron_assets::PlantModules::for_family(ctx.assets, &with_modules);
            let mut updated = saffron_vegetation::native_plant_family(
                id,
                &existing.name,
                graph,
                existing.material_slots.clone(),
                &module_resolver,
            )
            .map_err(Error::command)?;
            updated.modules = with_modules.modules.clone();
            if let saffron_vegetation::PlantFamilySource::Native {
                grafts: declared, ..
            } = &mut updated.source
            {
                *declared = grafts;
            }
            // Everything the artist authored around the graph survives the regrow. What the graph
            // derives — variations, appearances, proxies, dimensions, spines — is the NEW graph's,
            // never the previous one's leftovers.
            // A module family stays a module: the role and the depth budget are authored around the
            // graph, not derived from it, and a regrow that reset them would unbind every caller.
            updated.role = existing.role;
            updated.module_recursion_limit = existing.module_recursion_limit;
            updated.tags = existing.tags.clone();
            updated.mechanics = existing.mechanics;
            updated.interaction_policy = existing.interaction_policy;
            updated.habitat = existing.habitat.clone();
            updated.ecology = existing.ecology.clone();
            saffron_vegetation::validate_plant_family(&updated).map_err(Error::command)?;
            saffron_assets::update_plant_family_asset(ctx.assets, id, &updated)
                .map_err(Error::command)?;
            let saffron_vegetation::PlantFamilySource::Native { graph, grafts } = &updated.source
            else {
                return Err(Error::command("regrown family lost its botanical graph"));
            };
            Ok(saffron_protocol::PlantGraphResult {
                plant: WireUuid(id.value()),
                graph: crate::botanical_dto::graph_dto(graph),
                grafts: grafts
                    .iter()
                    .map(crate::botanical_dto::graft_source_dto)
                    .collect(),
                modules: updated
                    .modules
                    .iter()
                    .map(crate::botanical_dto::module_reference_dto)
                    .collect(),
                growth: plant_growth_dto(
                    ctx.assets,
                    &updated,
                    0,
                    &saffron_vegetation::BotanicalBudget::COOK,
                )?,
            })
        },
    );
    reg.register::<
        saffron_protocol::PlantPhenotypesParams,
        saffron_protocol::PlantPhenotypesResult,
    >(
        "plant-phenotypes",
        "plant-phenotypes {plant, phenotypes?} — read or replace one family's authored appearances",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let mut plant = load_plant_family_asset(ctx.assets, id)
                .map_err(Error::command)?;
            if let Some(phenotypes) = &params.phenotypes {
                plant.phenotypes = phenotypes
                    .iter()
                    .map(phenotype_from_dto)
                    .collect::<Result<Vec<_>>>()?;
                // The family validator is the authority, not this command: it is what knows a
                // phenotype must name a declared variation, that two on one variation may not share
                // a role, that a remap must move between real slots, and that a family needs a
                // healthy appearance. Rejecting here would be a second rule to keep in sync.
                saffron_vegetation::validate_plant_family(&plant)
                    .map_err(Error::command)?;
                saffron_assets::update_plant_family_asset(ctx.assets, id, &plant)
                    .map_err(Error::command)?;
            }
            Ok(saffron_protocol::PlantPhenotypesResult {
                plant: WireUuid(id.value()),
                phenotypes: plant.phenotypes.iter().map(phenotype_dto).collect(),
            })
        },
    );
    reg.register::<saffron_protocol::PlantProxiesParams, saffron_protocol::PlantProxiesResult>(
        "plant-proxies",
        "plant-proxies {plant} — the collision and navigation proxies a family derived",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
            let metres = |value: saffron_spatial::DecisionScalar| value.bits() as f32 / 65_536.0;
            Ok(saffron_protocol::PlantProxiesResult {
                plant: WireUuid(id.value()),
                collision: plant
                    .collision_proxies
                    .iter()
                    .map(|proxy| saffron_protocol::PlantCollisionProxyDto {
                        shape: match proxy.shape {
                            saffron_vegetation::PlantCollisionShape::Box => {
                                saffron_protocol::PlantCollisionShapeDto::Box
                            }
                            saffron_vegetation::PlantCollisionShape::Sphere => {
                                saffron_protocol::PlantCollisionShapeDto::Sphere
                            }
                            saffron_vegetation::PlantCollisionShape::Capsule => {
                                saffron_protocol::PlantCollisionShapeDto::Capsule
                            }
                            saffron_vegetation::PlantCollisionShape::ConvexHull => {
                                saffron_protocol::PlantCollisionShapeDto::ConvexHull
                            }
                        },
                        center_m: [
                            metres(proxy.center[0]),
                            metres(proxy.center[1]),
                            metres(proxy.center[2]),
                        ],
                        dimensions_m: [
                            metres(proxy.dimensions[0]),
                            metres(proxy.dimensions[1]),
                            metres(proxy.dimensions[2]),
                        ],
                        breakable: proxy.breakable,
                    })
                    .collect(),
                navigation: plant
                    .navigation_proxies
                    .iter()
                    .map(|proxy| saffron_protocol::PlantNavigationProxyDto {
                        footprint_m: proxy
                            .footprint
                            .iter()
                            .map(|point| [metres(point[0]), metres(point[1])])
                            .collect(),
                        height_m: metres(proxy.height),
                        cost: f32::from(proxy.cost.bits()) / 65_535.0,
                    })
                    .collect(),
            })
        },
    );

    reg.register::<
        saffron_protocol::PlantSeasonPhenotypeParams,
        saffron_protocol::PlantSeasonPhenotypeResult,
    >(
        "plant-season-phenotype",
        "plant-season-phenotype {plant, seasonMille, lifecycle?, healthMille?, moistureMille?} — the appearance a family renders then, with every phenotype's weight",
        |ctx, params| {
            require_project_loaded(ctx)?;
            if params.season_mille > 999 {
                return Err(Error::command("seasonMille is per-mille of the year (0..1000)"));
            }
            let unit = |value: Option<u32>, default: saffron_spatial::UnitInterval| match value {
                Some(value) if value <= 1000 => Ok(saffron_spatial::UnitInterval::from_bits(
                    (value * u32::from(u16::MAX) / 1000) as u16,
                )),
                Some(_) => Err(Error::command("health and moisture are per-mille (0..=1000)")),
                None => Ok(default),
            };
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(Error::command)?;
            let state = saffron_vegetation::PhenologyState {
                lifecycle: params.lifecycle.map_or(
                    saffron_vegetation::PlantLifecycle::Mature,
                    crate::commands_vegetation_runtime::lifecycle_from_dto,
                ),
                season_mille: u16::try_from(params.season_mille).unwrap_or(0),
                health: unit(params.health_mille, saffron_spatial::UnitInterval::ONE)?,
                moisture: unit(params.moisture_mille, saffron_spatial::UnitInterval::ZERO)?,
            };
            // The engine's own resolver, not a second reading of the same rules: lifecycle wins
            // over the curves (a dead plant does not turn autumnal), and the cooked phenotype is
            // the fallback throughout. A preview that resolved this differently from the renderer
            // would be showing an appearance the scene never picks.
            let phenotype = saffron_vegetation::resolve_rendered_phenotype(
                plant
                    .phenotypes
                    .iter()
                    .map(|entry| (entry.id, entry.role, entry.response)),
                plant.phenotypes.first().map_or(0, |entry| entry.id),
                state,
            );
            let variation = plant
                .phenotypes
                .iter()
                .find(|entry| entry.id == phenotype)
                .map_or(0, |entry| entry.variation);
            Ok(saffron_protocol::PlantSeasonPhenotypeResult {
                plant: WireUuid(id.value()),
                phenotype,
                variation,
                weights: plant
                    .phenotypes
                    .iter()
                    .map(|entry| saffron_protocol::PlantPhenotypeWeightDto {
                        phenotype: entry.id,
                        role: crate::commands_asset::preview::phenotype_role_dto(entry.role),
                        weight_mille: saffron_vegetation::phenotype_weight_mille(
                            entry.role,
                            entry.response,
                            state,
                        )
                        .map(u32::from),
                    })
                    .collect(),
            })
        },
    );

    reg.register::<saffron_protocol::PlantHierarchyParams, saffron_protocol::PlantHierarchyResult>(
        "plant-hierarchy",
        "plant-hierarchy {plant} — the cooked cut: each node's representation, page, and declared error",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(Error::command)?;
            let options = saffron_assets::PlantRecookOptions {
                limits: PlantCompileLimits::default(),
                versions: vegetation_cook_versions(),
                platform: portable_vegetation_platform_profile(None),
            };
            let published = match recook_plant_family(ctx.assets, &plant, &options)
                .map_err(Error::from)?
            {
                saffron_assets::PlantRecookOutcome::Published(published) => published,
                saffron_assets::PlantRecookOutcome::Rejected(_) => {
                    return Err(Error::command(
                        "plant family does not validate — run plant-validate for diagnostics",
                    ));
                }
            };
            let hierarchy = saffron_assets::plant_family_hierarchy(
                ctx.assets,
                published.publication.content_hash,
            )
            .map_err(Error::from)?;
            // The walk goes down from the roots rather than reading each node's parent in array
            // order: the cooker emits leaves before the roots that own them, so an array-order pass
            // would read a parent's depth before it is set and report every node one level too deep.
            let mut depth = vec![0_u32; hierarchy.nodes.len()];
            let mut frontier: Vec<u32> = hierarchy.roots.clone();
            let mut visited = vec![false; hierarchy.nodes.len()];
            for root in &frontier {
                if let Some(seen) = visited.get_mut(*root as usize) {
                    *seen = true;
                }
            }
            while let Some(index) = frontier.pop() {
                let Some(node) = hierarchy.nodes.get(index as usize) else {
                    continue;
                };
                let child_depth = depth[index as usize].saturating_add(1);
                for child in &node.children {
                    // The guard is against a malformed tree, not an expected shape: a cycle would
                    // otherwise spin here forever.
                    match visited.get_mut(*child as usize) {
                        Some(seen) if !*seen => *seen = true,
                        _ => continue,
                    }
                    depth[*child as usize] = child_depth;
                    frontier.push(*child);
                }
            }
            let mut triangle_nodes = 0;
            let mut voxel_nodes = 0;
            let nodes = hierarchy
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| {
                    let (representation, primitives) = match node.representation {
                        saffron_geometry::HierarchyRepresentation::Triangles { count, .. } => {
                            triangle_nodes += 1;
                            ("triangles", count)
                        }
                        saffron_geometry::HierarchyRepresentation::Voxel { brick } => {
                            voxel_nodes += 1;
                            let triangles = hierarchy
                                .voxel_bricks
                                .get(brick as usize)
                                .map_or(0, |brick| {
                                    u32::try_from(brick.indices.len() / 3).unwrap_or(u32::MAX)
                                });
                            ("voxel", triangles)
                        }
                    };
                    saffron_protocol::PlantHierarchyNodeDto {
                        id: node.id,
                        parent: node.parent,
                        depth: depth.get(index).copied().unwrap_or(0),
                        representation: representation.to_owned(),
                        primitives,
                        page: node.page,
                        child_count: u32::try_from(node.children.len()).unwrap_or(u32::MAX),
                        appearance_error: saffron_protocol::AppearanceErrorDto {
                            silhouette: node.appearance_error.silhouette,
                            coverage: node.appearance_error.coverage,
                            transmission: node.appearance_error.transmission,
                            material: node.appearance_error.material,
                            normal_distribution: node.appearance_error.normal_distribution,
                            total: node.appearance_error.total,
                        },
                    }
                })
                .collect();
            Ok(saffron_protocol::PlantHierarchyResult {
                plant: WireUuid(id.value()),
                triangle_nodes,
                voxel_nodes,
                nodes,
            })
        },
    );

    reg.register::<saffron_protocol::PlantAtlasParams, saffron_protocol::PlantAtlasResult>(
        "plant-atlas",
        "plant-atlas {plant, level?} — one cooked family's packed coverage atlas as a PNG",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
            let options = saffron_assets::PlantRecookOptions {
                limits: PlantCompileLimits::default(),
                versions: vegetation_cook_versions(),
                platform: portable_vegetation_platform_profile(None),
            };
            // Recooking is a cache hit for a family already cooked, so this reads the published
            // artifact rather than packing a second atlas — the family's UVs address exactly one
            // layout, and a fresh packing would not be it.
            let published =
                match recook_plant_family(ctx.assets, &plant, &options).map_err(Error::from)? {
                    saffron_assets::PlantRecookOutcome::Published(published) => published,
                    saffron_assets::PlantRecookOutcome::Rejected(_) => {
                        return Err(Error::command(
                            "plant family does not validate — run plant-validate for diagnostics",
                        ));
                    }
                };
            let atlas = saffron_assets::plant_family_atlas_image(
                ctx.assets,
                published.publication.content_hash,
                params.level,
            )
            .map_err(Error::from)?
            .ok_or_else(|| {
                Error::command(
                    "this family cooked no packed atlas: its slots resolve to catalog materials",
                )
            })?;
            Ok(saffron_protocol::PlantAtlasResult {
                plant: WireUuid(id.value()),
                level: params.level,
                level_count: atlas.level_count,
                width: atlas.width,
                height: atlas.height,
                gutter: atlas.gutter,
                placements: atlas
                    .placements
                    .iter()
                    .map(|placement| saffron_protocol::PlantAtlasPlacementDto {
                        slot: placement.slot,
                        x: placement.x,
                        y: placement.y,
                        width: placement.width,
                        height: placement.height,
                    })
                    .collect(),
                base64: base64_encode(&atlas.png),
            })
        },
    );

    reg.register::<saffron_protocol::PlantGrowthParams, saffron_protocol::PlantElementsResult>(
        "plant-elements",
        "every element of one native plant family a manual edit can address",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
            let saffron_vegetation::PlantFamilySource::Native { graph, .. } = &plant.source else {
                return Err(Error::command(
                    "plant family has an imported source, not a botanical graph",
                ));
            };
            let modules = saffron_assets::PlantModules::for_family(ctx.assets, &plant);
            let growth = saffron_vegetation::grow(
                graph,
                params.variation as usize,
                &modules,
                &saffron_vegetation::BotanicalBudget::COOK,
            )
            .map_err(Error::command)?;
            let frame_axis: BTreeMap<_, _> = growth
                .assembly
                .frames
                .iter()
                .map(|frame| (frame.id, frame.axis))
                .collect();
            let elements = growth
                .assembly
                .elements
                .iter()
                .map(|placement| {
                    let axis = frame_axis.get(&placement.frame).copied().ok_or_else(|| {
                        Error::command("grown element sits on a frame the growth does not carry")
                    })?;
                    Ok(crate::botanical_dto::placement_dto(placement, axis))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(saffron_protocol::PlantElementsResult {
                plant: WireUuid(id.value()),
                axes: growth
                    .assembly
                    .axes
                    .iter()
                    .map(crate::botanical_dto::axis_dto)
                    .collect(),
                elements,
            })
        },
    );
    reg.register::<saffron_protocol::PlantGrowthParams, saffron_protocol::BotanicalGrowthDto>(
        "plant-growth",
        "what one plant family's botanical graph grows",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
            plant_growth_dto(
                ctx.assets,
                &plant,
                params.variation,
                &preview_budget(params.max_axes, params.max_elements),
            )
        },
    );
    reg.register::<PlantValidateParams, PlantValidationResult>(
        "plant-validate",
        "validate one authored plant family and inspect its exact sources",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
            let outcome =
                validate_plant_family_sources(ctx.assets, &plant, PlantCompileLimits::default())
                    .map_err(Error::from)?;
            Ok(PlantValidationResult {
                plant: WireUuid(id.value()),
                validation: plant_validation_summary(&outcome),
                diagnostics: outcome
                    .compile
                    .diagnostics
                    .iter()
                    .map(plant_compile_diagnostic_dto)
                    .collect(),
                sources: plant_sources_dto(&plant, &outcome),
                dependencies: manifest_dependencies_dto(&outcome.dependencies),
                conflicts: outcome
                    .compile
                    .conflicts
                    .conflicts
                    .iter()
                    .map(plant_reimport_conflict_dto)
                    .collect(),
                source_updates: plant_source_updates_dto(&outcome),
                family_hash: outcome.compile.family_hash.as_ref().map(coverage_hash_text),
                statistics: plant_compile_statistics_dto(outcome.compile.statistics),
            })
        },
    );

    reg.register::<PlantRecookParams, PlantRecookResult>(
        "plant-recook",
        "normalize and atomically publish one plant family through its retained recipe",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
            if params
                .platform_profile
                .as_deref()
                .is_some_and(str::is_empty)
            {
                return Err(Error::command("platformProfile cannot be empty"));
            }
            let options = PlantRecookOptions {
                limits: PlantCompileLimits::default(),
                versions: vegetation_cook_versions(),
                platform: portable_vegetation_platform_profile(params.platform_profile.as_deref()),
            };
            match recook_plant_family(ctx.assets, &plant, &options).map_err(Error::from)? {
                PlantRecookOutcome::Published(published) => {
                    let family_hash = published
                        .validation
                        .compile
                        .family_hash
                        .as_ref()
                        .map(coverage_hash_text)
                        .ok_or_else(|| {
                            Error::command("plant compiler published without a family hash")
                        })?;
                    Ok(PlantRecookResult {
                        plant: WireUuid(id.value()),
                        family_hash,
                        artifact_hash: published.publication.content_hash.to_string(),
                        cache_hit: published.publication.cache_hit,
                        validation: plant_validation_summary(&published.validation),
                        diagnostics: published
                            .validation
                            .compile
                            .diagnostics
                            .iter()
                            .map(plant_compile_diagnostic_dto)
                            .collect(),
                        sources: plant_sources_dto(
                            &published.accepted_asset,
                            &published.validation,
                        ),
                        dependencies: manifest_dependencies_dto(&published.validation.dependencies),
                        source_updates: plant_source_updates_dto(&published.validation),
                        statistics: plant_compile_statistics_dto(
                            published.validation.compile.statistics,
                        ),
                    })
                }
                PlantRecookOutcome::Rejected(rejected) => {
                    let conflicts = rejected
                        .compile
                        .conflicts
                        .conflicts
                        .iter()
                        .map(plant_reimport_conflict_dto)
                        .collect::<Vec<_>>();
                    if !conflicts.is_empty() {
                        return Err(Error::Diagnostic {
                            message: format!("plant family {} has reimport conflicts", id.value()),
                            diagnostic: Box::new(ControlDiagnosticDto::ReimportConflict(
                                ReimportConflictDiagnosticDto {
                                    plant: WireUuid(id.value()),
                                    conflicts,
                                },
                            )),
                        });
                    }
                    let message = rejected
                        .compile
                        .diagnostics
                        .iter()
                        .find(|diagnostic| {
                            diagnostic.severity == PlantCompileDiagnosticSeverity::Error
                        })
                        .map(|diagnostic| diagnostic.message.clone())
                        .unwrap_or_else(|| {
                            format!("plant family {} failed validation", id.value())
                        });
                    Err(Error::command(message))
                }
            }
        },
    );
}
