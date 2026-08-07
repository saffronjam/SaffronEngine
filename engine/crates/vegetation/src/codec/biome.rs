//! The `.sbiome` biome-graph document.

use super::enums::{biome_parameter_type, biome_parameter_type_tag};
use super::stream::{Reader, Writer};
use crate::hash::sha256;
use crate::*;

const BIOME_MAGIC: &[u8; 8] = b"SBIOME01";

/// SHA-256 identity of the `.sbiome` binary field vocabulary.
#[must_use]
pub fn biome_asset_schema_hash() -> [u8; 32] {
    sha256(b"saffron-anima/sbiome/schema/v1/role+parameters+palette+density+clustering+suitability+competition+companions+succession+seeds+modules+policy+graph")
}

/// Writes one root biome or reusable module to canonical `.sbiome` bytes.
pub fn write_biome_asset(asset: &BiomeAsset) -> Result<Vec<u8>> {
    validate_biome(asset)?;
    let mut writer = Writer::with_header(BIOME_MAGIC, asset.version, biome_asset_schema_hash());
    writer.uuid(asset.id);
    writer.string(&asset.name)?;
    writer.u8(match asset.role {
        BiomeRole::Root => 0,
        BiomeRole::Module => 1,
    });
    writer.vec(&asset.parameters, |writer, parameter| {
        writer.u128(parameter.id);
        writer.string(&parameter.name)?;
        writer.u8(biome_parameter_type_tag(parameter.parameter_type));
        writer.value(&parameter.default_value)
    })?;
    writer.vec(&asset.palette, |writer, entry| {
        writer.uuid(entry.plant);
        writer.unit(entry.weight);
        writer.u128(entry.seed_namespace);
        Ok(())
    })?;
    writer.fixed(asset.density);
    writer.unit(asset.clustering);
    writer.vec(&asset.suitability, |writer, binding| {
        writer.field_channel(binding.channel);
        writer.fixed(binding.minimum);
        writer.fixed(binding.maximum);
        writer.fixed(binding.falloff);
        writer.u128(binding.node_guid);
        Ok(())
    })?;
    writer.vec(&asset.competition, |writer, rule| {
        writer.uuid(rule.first);
        writer.uuid(rule.second);
        writer.fixed(rule.spacing);
        writer.i32(rule.priority);
        Ok(())
    })?;
    writer.vec(&asset.companions, |writer, rule| {
        writer.uuid(rule.parent);
        writer.uuid(rule.child);
        writer.fixed(rule.minimum_distance);
        writer.fixed(rule.maximum_distance);
        writer.unit(rule.probability);
        Ok(())
    })?;
    writer.vec(&asset.succession, |writer, rule| {
        writer.uuid(rule.from);
        writer.uuid(rule.to);
        writer.u64(rule.minimum_tick);
        writer.unit(rule.probability);
        Ok(())
    })?;
    writer.vec(&asset.seed_namespaces, |writer, (name, namespace)| {
        writer.string(name)?;
        writer.u128(*namespace);
        Ok(())
    })?;
    writer.vec(&asset.modules, |writer, module| {
        writer.uuid(module.biome);
        writer.u128(module.call_guid);
        writer.vec(&module.bindings, |writer, (parameter, value)| {
            writer.u128(*parameter);
            writer.value(value)
        })
    })?;
    writer.u16(asset.policy.maximum_recursion);
    writer.fixed(asset.policy.maximum_influence_radius);
    writer.bool(asset.policy.require_authoritative_fields);
    writer.value(&asset.graph)?;
    Ok(writer.finish())
}

/// Reads and strictly validates one canonical `.sbiome` byte stream.
pub fn read_biome_asset(bytes: &[u8]) -> Result<BiomeAsset> {
    let mut reader = Reader::with_header(
        bytes,
        BIOME_MAGIC,
        BIOME_ASSET_VERSION,
        biome_asset_schema_hash(),
        ".sbiome",
    )?;
    let asset = BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id: reader.uuid()?,
        name: reader.string()?,
        role: match reader.u8()? {
            0 => BiomeRole::Root,
            1 => BiomeRole::Module,
            _ => return Err(reader.invalid("role")),
        },
        parameters: reader.vec(|reader| {
            Ok(BiomeParameter {
                id: reader.u128()?,
                name: reader.string()?,
                parameter_type: biome_parameter_type(reader.u8()?)?,
                default_value: reader.value()?,
            })
        })?,
        palette: reader.vec(|reader| {
            Ok(BiomePaletteEntry {
                plant: reader.uuid()?,
                weight: reader.unit()?,
                seed_namespace: reader.u128()?,
            })
        })?,
        density: reader.fixed()?,
        clustering: reader.unit()?,
        suitability: reader.vec(|reader| {
            Ok(SuitabilityBinding {
                channel: reader.field_channel()?,
                minimum: reader.fixed()?,
                maximum: reader.fixed()?,
                falloff: reader.fixed()?,
                node_guid: reader.u128()?,
            })
        })?,
        competition: reader.vec(|reader| {
            Ok(CompetitionRule {
                first: reader.uuid()?,
                second: reader.uuid()?,
                spacing: reader.fixed()?,
                priority: reader.i32()?,
            })
        })?,
        companions: reader.vec(|reader| {
            Ok(CompanionRule {
                parent: reader.uuid()?,
                child: reader.uuid()?,
                minimum_distance: reader.fixed()?,
                maximum_distance: reader.fixed()?,
                probability: reader.unit()?,
            })
        })?,
        succession: reader.vec(|reader| {
            Ok(SuccessionRule {
                from: reader.uuid()?,
                to: reader.uuid()?,
                minimum_tick: reader.u64()?,
                probability: reader.unit()?,
            })
        })?,
        seed_namespaces: reader.vec(|reader| Ok((reader.string()?, reader.u128()?)))?,
        modules: reader.vec(|reader| {
            Ok(BiomeModuleReference {
                biome: reader.uuid()?,
                call_guid: reader.u128()?,
                bindings: reader.vec(|reader| Ok((reader.u128()?, reader.value()?)))?,
            })
        })?,
        policy: BiomeGraphPolicy {
            maximum_recursion: reader.u16()?,
            maximum_influence_radius: reader.fixed()?,
            require_authoritative_fields: reader.bool()?,
        },
        graph: reader.value()?,
    };
    reader.complete()?;
    validate_biome(&asset)?;
    Ok(asset)
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;
    use saffron_json::Value;
    use saffron_spatial::UnitInterval;

    use super::*;
    use crate::codec::fixed;

    #[test]
    fn biome_asset_round_trips_canonical_bytes() {
        let asset = BiomeAsset {
            version: BIOME_ASSET_VERSION,
            id: Uuid(21),
            name: "Temperate forest".to_owned(),
            role: BiomeRole::Root,
            parameters: vec![BiomeParameter {
                id: 22,
                name: "canopy".to_owned(),
                parameter_type: BiomeParameterType::Unit,
                default_value: Value::from(1),
            }],
            palette: vec![BiomePaletteEntry {
                plant: Uuid(11),
                weight: UnitInterval::ONE,
                seed_namespace: 23,
            }],
            density: fixed(1),
            clustering: UnitInterval::from_bits(24),
            suitability: Vec::new(),
            competition: Vec::new(),
            companions: Vec::new(),
            succession: Vec::new(),
            seed_namespaces: vec![("canopy".to_owned(), 23)],
            modules: Vec::new(),
            policy: BiomeGraphPolicy {
                maximum_recursion: 8,
                maximum_influence_radius: fixed(64),
                require_authoritative_fields: true,
            },
            graph: Value::Object(Default::default()),
        };
        let bytes = write_biome_asset(&asset).unwrap();
        let decoded = read_biome_asset(&bytes).unwrap();
        assert_eq!(decoded, asset);
        assert_eq!(write_biome_asset(&decoded).unwrap(), bytes);
    }
}
