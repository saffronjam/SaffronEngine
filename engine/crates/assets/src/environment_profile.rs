//! Native `.senv` environment-profile assets and the built-in starting profiles.

use saffron_core::Uuid;
use saffron_scene::{
    AssetEntry, AssetType, SceneEnvironment, environment_from_json, environment_to_json,
};

use crate::import::hash_bytes_fnv;
use crate::{AssetServer, Error, Result};

/// A built-in complete environment available through the same apply path as project profiles.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltinEnvironmentProfile {
    /// Stable wire key.
    pub key: &'static str,
    /// Editor-facing name.
    pub name: &'static str,
    /// Complete environment value.
    pub environment: SceneEnvironment,
}

/// The built-in complete environments in browser order.
#[must_use]
pub fn builtin_environment_profiles() -> Vec<BuiltinEnvironmentProfile> {
    let neutral = SceneEnvironment::default();

    let mut clear_day = SceneEnvironment::default();
    clear_day.atmosphere.enabled = true;
    clear_day.time_of_day.enabled = true;
    clear_day.time_of_day.time_of_day = 0.5;

    let mut golden_hour = clear_day.clone();
    golden_hour.time_of_day.time_of_day = 0.75;
    golden_hour.cloud.enabled = true;
    golden_hour.cloud.coverage = 0.18;
    golden_hour.cloud.cloud_type = 0.65;

    let mut overcast = clear_day.clone();
    overcast.cloud.enabled = true;
    overcast.cloud.coverage = 0.92;
    overcast.cloud.cloud_type = 0.28;
    overcast.cloud.precipitation = 0.15;
    overcast.fog.enabled = true;
    overcast.fog.density = 0.008;
    overcast.fog.max_opacity = 0.65;

    let mut night = clear_day.clone();
    night.time_of_day.time_of_day = 0.02;
    night.sky_intensity = 0.4;
    night.ambient_intensity = 0.05;
    night.atmosphere.sun_disk_intensity = 0.15;
    night.atmosphere.moon_disk_intensity = 2.0;
    night.atmosphere.moon_earthshine = 0.08;

    vec![
        BuiltinEnvironmentProfile {
            key: "neutral",
            name: "Neutral",
            environment: neutral,
        },
        BuiltinEnvironmentProfile {
            key: "clear-day",
            name: "Clear day",
            environment: clear_day,
        },
        BuiltinEnvironmentProfile {
            key: "golden-hour",
            name: "Golden hour",
            environment: golden_hour,
        },
        BuiltinEnvironmentProfile {
            key: "overcast",
            name: "Overcast",
            environment: overcast,
        },
        BuiltinEnvironmentProfile {
            key: "night",
            name: "Night",
            environment: night,
        },
    ]
}

/// Resolves a built-in environment by its stable key.
#[must_use]
pub fn builtin_environment_profile(key: &str) -> Option<BuiltinEnvironmentProfile> {
    builtin_environment_profiles()
        .into_iter()
        .find(|profile| profile.key == key)
}

/// Reads a complete environment profile from the catalog.
///
/// # Errors
///
/// Returns a typed catalog, I/O, or JSON error when the profile cannot be read.
pub fn load_environment_profile(assets: &AssetServer, id: Uuid) -> Result<SceneEnvironment> {
    let entry = assets
        .catalog
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != AssetType::Environment {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "environment",
        });
    }
    let text = std::fs::read_to_string(assets.root.join(&entry.path))
        .map_err(|error| Error::Io(error.to_string()))?;
    let document = saffron_json::parse_json(&text)?;
    Ok(environment_from_json(&document))
}

/// Writes a new `.senv` and registers it in the asset catalog.
///
/// # Errors
///
/// Returns [`Error::Io`] if the profile or sidecar cannot be written.
pub fn save_environment_profile(
    assets: &mut AssetServer,
    environment: &SceneEnvironment,
    name: &str,
    folder: &str,
) -> Result<Uuid> {
    let id = Uuid::new();
    assets.ensure_asset_directories();
    let relative_path = format!("environments/{}.senv", id.value());
    let text = saffron_json::dump_json_sorted(&environment_to_json(environment), 2);
    std::fs::write(assets.root.join(&relative_path), &text)
        .map_err(|error| Error::Io(error.to_string()))?;
    let unique_name = assets.catalog.unique_name(name);
    assets.register_imported_asset(AssetEntry {
        id,
        name: unique_name,
        asset_type: AssetType::Environment,
        path: relative_path,
        folder: folder.to_owned(),
        content_hash: hash_bytes_fnv(text.as_bytes()),
        ..AssetEntry::default()
    });
    assets.write_asset_sidecar(id)?;
    Ok(id)
}

/// Overwrites an existing `.senv` while preserving its catalog identity.
///
/// # Errors
///
/// Returns a typed catalog or I/O error when the profile cannot be updated.
pub fn update_environment_profile(
    assets: &mut AssetServer,
    id: Uuid,
    environment: &SceneEnvironment,
) -> Result<()> {
    let relative_path = {
        let entry = assets
            .catalog
            .find(id)
            .ok_or(Error::NotInCatalog(id.value()))?;
        if entry.asset_type != AssetType::Environment {
            return Err(Error::WrongAssetType {
                id: id.value(),
                wanted: "environment",
            });
        }
        entry.path.clone()
    };
    let text = saffron_json::dump_json_sorted(&environment_to_json(environment), 2);
    std::fs::write(assets.root.join(relative_path), &text)
        .map_err(|error| Error::Io(error.to_string()))?;
    let hash = hash_bytes_fnv(text.as_bytes());
    let updated = assets.update_asset_content_hash(id, hash);
    debug_assert!(updated, "validated environment remains catalogued");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_profile_round_trips_through_catalog_asset() {
        let root =
            std::env::temp_dir().join(format!("saffron-env-profile-{}", Uuid::new().value()));
        let mut assets = AssetServer::new(&root);
        let mut expected = SceneEnvironment::default();
        expected.atmosphere.enabled = true;
        expected.cloud.coverage = 0.73;

        let id = save_environment_profile(&mut assets, &expected, "Studio", "looks")
            .expect("save environment profile");
        assert_eq!(
            load_environment_profile(&assets, id).expect("load profile"),
            expected
        );
        assert_eq!(
            assets.catalog.find(id).expect("catalog row").folder,
            "looks"
        );

        expected.fog.enabled = true;
        update_environment_profile(&mut assets, id, &expected).expect("update profile");
        assert_eq!(
            load_environment_profile(&assets, id).expect("reload profile"),
            expected
        );
        std::fs::remove_dir_all(root).expect("remove test asset root");
    }

    #[test]
    fn builtins_are_complete_and_keyed_uniquely() {
        let profiles = builtin_environment_profiles();
        assert_eq!(profiles.len(), 5);
        let mut keys = profiles
            .iter()
            .map(|profile| profile.key)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), profiles.len());
        assert_eq!(
            builtin_environment_profile("neutral")
                .expect("neutral builtin")
                .environment,
            SceneEnvironment::default()
        );
    }
}
