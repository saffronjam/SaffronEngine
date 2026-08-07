use saffron_protocol::{
    EmptyParams, ListProbesResult, ProbeRef, RecaptureProbesResult, SetProbesParams,
    SetProbesResult, Uuid,
};
use saffron_scene::ReflectionProbe;

use super::*;
use crate::registry::CommandRegistry;

/// Registers reflection-probe management.
pub(crate) fn register_probes(reg: &mut CommandRegistry) {
    reg.register::<SetProbesParams, SetProbesResult>(
        "set-probes",
        "set-probes {0|1} — toggle reflection-probe specular sampling",
        |ctx, params| {
            ctx.renderer
                .set_reflection_probes(params.enabled.unwrap_or(true));
            Ok(SetProbesResult {
                probes: ctx.renderer.reflection_probes_enabled(),
            })
        },
    );

    reg.register::<EmptyParams, RecaptureProbesResult>(
        "recapture-probes",
        "recapture-probes — mark every reflection probe dirty (forces re-capture)",
        |ctx, _params| {
            let mut marked = 0u32;
            ctx.scene_edit
                .active_scene()
                .for_each::<(&mut ReflectionProbe,), _>(|_, (probe,)| {
                    probe.dirty = true;
                    marked += 1;
                });
            Ok(RecaptureProbesResult { marked })
        },
    );

    reg.register::<EmptyParams, ListProbesResult>(
        "list-probes",
        "list-probes — captured reflection probes (origin/radius/intensity/valid)",
        |ctx, _params| {
            let enabled = ctx.renderer.reflection_probes_enabled();
            let probes: Vec<ProbeRef> = ctx
                .renderer
                .reflection_probes()
                .iter()
                .enumerate()
                .map(|(slot, probe)| ProbeRef {
                    slot: slot as u32,
                    entity: Uuid::from(probe.entity),
                    origin: to_vec3(probe.origin),
                    influence_radius: probe.influence_radius,
                    intensity: probe.intensity,
                    box_projection: probe.box_projection,
                    valid: probe.valid,
                    dirty: probe.dirty,
                })
                .collect();
            Ok(ListProbesResult {
                enabled,
                count: probes.len() as u32,
                probes,
            })
        },
    );
}
