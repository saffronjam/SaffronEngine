use saffron_protocol::{
    EmptyParams, ResidencyFacetDto, VegetationNetworkInterestCellDto,
    VegetationNetworkInterestParams, VegetationNetworkSessionResult,
};
use saffron_spatial::{ResidencyFacet, ResidencyMask};
use saffron_vegetation::{CellInterestSet, VEGETATION_NETWORK_PROTOCOL_VERSION};

use super::{facets_dto, parse_cell, require_runtime, runtime, runtime_mut};
use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, EngineContext};

/// Registers the network-session scope declaration and its checkpoint fingerprint.
pub(crate) fn register_runtime_network(reg: &mut CommandRegistry) {
    reg.register::<VegetationNetworkInterestParams, VegetationNetworkSessionResult>(
        "vegetation-network-interest",
        "declare the cells and facets this world is seated with in a network session",
        |ctx, params| {
            require_runtime(ctx)?;
            let interest = if params.cells.is_empty() {
                None
            } else {
                let mut set = CellInterestSet::new();
                for entry in &params.cells {
                    let mut mask = ResidencyMask::NONE;
                    for facet in &entry.facets {
                        mask = mask.with(parse_facet(*facet));
                    }
                    set.declare(parse_cell(&entry.cell)?, mask)
                        .map_err(Error::from)?;
                }
                Some(set)
            };
            runtime_mut(ctx)?.declare_network_interest(interest);
            session_result(ctx)
        },
    );
    reg.register::<EmptyParams, VegetationNetworkSessionResult>(
        "vegetation-network-checkpoint",
        "fingerprint the seated scope at the highest agreed transport sequence",
        |ctx, _| {
            require_runtime(ctx)?;
            session_result(ctx)
        },
    );
}

fn parse_facet(facet: ResidencyFacetDto) -> ResidencyFacet {
    match facet {
        ResidencyFacetDto::Render => ResidencyFacet::Render,
        ResidencyFacetDto::Physics => ResidencyFacet::Physics,
        ResidencyFacetDto::Simulation => ResidencyFacet::Simulation,
        ResidencyFacetDto::Editing => ResidencyFacet::Editing,
        ResidencyFacetDto::Navigation => ResidencyFacet::Navigation,
        ResidencyFacetDto::Network => ResidencyFacet::Network,
    }
}

fn session_result(ctx: &mut EngineContext<'_>) -> Result<VegetationNetworkSessionResult> {
    let world = runtime(ctx)?;
    let manifest_identity = world.manifest_identity().to_string();
    let sequence = world.network_sequence().to_string();
    let Some(interest) = world.network_interest() else {
        return Ok(VegetationNetworkSessionResult {
            seated: false,
            protocol_version: VEGETATION_NETWORK_PROTOCOL_VERSION,
            sequence,
            interest: Vec::new(),
            interest_identity: None,
            state_identity: None,
            manifest_identity,
            ecology_tick: world
                .persistent_state()
                .ecology()
                .clock()
                .tick()
                .to_string(),
        });
    };
    let declared = interest
        .entries()
        .iter()
        .map(|(cell, mask)| VegetationNetworkInterestCellDto {
            cell: crate::vegetation_cook_dto::world_cell_dto(*cell),
            facets: facets_dto(*mask),
        })
        .collect();
    let checkpoint = world.network_checkpoint().map_err(Error::from)?;
    Ok(VegetationNetworkSessionResult {
        seated: true,
        protocol_version: VEGETATION_NETWORK_PROTOCOL_VERSION,
        sequence,
        interest: declared,
        interest_identity: Some(checkpoint.interest_identity.to_string()),
        state_identity: Some(checkpoint.state_identity.to_string()),
        manifest_identity,
        ecology_tick: checkpoint.ecology_tick.to_string(),
    })
}
