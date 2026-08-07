use std::collections::BTreeSet;

use super::records::matrix_is_finite;
use super::table::SceneTable;
use super::*;
use crate::GpuHandle;

impl PersistentGpuScene {
    pub(super) fn validate_prototype(
        &self,
        record: &GpuScenePrototypeRecord,
    ) -> Result<(), GpuSceneError> {
        validate_device_handle(record.geometry, "geometry")?;
        for &material in record.materials.iter() {
            require(&self.shared.materials, material, "material")?;
        }
        if let Some(deformation) = record.deformation {
            require(&self.shared.deformations, deformation, "deformation")?;
        }
        for &sdf in record.sdfs.iter() {
            require(&self.shared.sdfs, sdf, "SDF")?;
        }
        require(&self.shared.pages, record.root_page, "page")?;
        if record.bounds.into_iter().any(|value| !value.is_finite()) {
            return Err(GpuSceneError::NonFinite("prototype bounds"));
        }
        if record.bounds[3] < 0.0 {
            return Err(GpuSceneError::NegativeBoundsRadius);
        }
        Ok(())
    }

    pub(super) fn validate_instance(
        &self,
        record: &GpuSceneInstanceRecord,
    ) -> Result<(), GpuSceneError> {
        let prototype = require(&self.shared.prototypes, record.prototype, "prototype")?;
        if let Some(deformation) = record.deformation {
            require(&self.shared.deformations, deformation, "deformation")?;
        }
        self.validate_instance_against_prototype(record, prototype)
    }

    pub(super) fn validate_instance_against_prototype(
        &self,
        record: &GpuSceneInstanceRecord,
        prototype: &GpuScenePrototypeRecord,
    ) -> Result<(), GpuSceneError> {
        let mut prior = None;
        for material_override in record.material_overrides.iter() {
            if prior.is_some_and(|slot| slot >= material_override.slot) {
                return Err(GpuSceneError::MaterialOverrideOrder);
            }
            let slot = material_override.slot as usize;
            if slot >= prototype.materials.len() {
                return Err(GpuSceneError::MaterialOverrideSlot {
                    slot: material_override.slot,
                    count: prototype.materials.len(),
                });
            }
            require(
                &self.shared.materials,
                material_override.material,
                "material",
            )?;
            prior = Some(material_override.slot);
        }
        if let GpuSceneTransform::Dynamic(transform) = record.transform
            && (!matrix_is_finite(transform.current) || !matrix_is_finite(transform.previous))
        {
            return Err(GpuSceneError::NonFinite("dynamic transform"));
        }
        Ok(())
    }

    pub(super) fn validate_page(
        &self,
        updating: Option<GpuScenePageHandle>,
        record: &GpuScenePageRecord,
    ) -> Result<(), GpuSceneError> {
        validate_device_handle(record.table, "page")?;
        let Some(mut parent) = record.parent else {
            return Ok(());
        };
        let mut seen = BTreeSet::new();
        while let Some(page) = self.shared.pages.get(parent) {
            if Some(parent) == updating || !seen.insert(parent) {
                return Err(GpuSceneError::PageCycle);
            }
            match page.parent {
                Some(next) => parent = next,
                None => return Ok(()),
            }
        }
        Err(GpuSceneError::StaleHandle {
            kind: "page",
            handle: parent.raw(),
        })
    }

    pub(super) fn material_is_referenced(&self, handle: GpuSceneMaterialHandle) -> bool {
        self.shared
            .prototypes
            .iter()
            .any(|(_, prototype)| prototype.materials.contains(&handle))
            || self
                .worlds
                .values()
                .flat_map(|world| world.instances.iter())
                .any(|(_, instance)| {
                    instance
                        .material_overrides
                        .iter()
                        .any(|material_override| material_override.material == handle)
                })
    }

    pub(super) fn deformation_is_referenced(&self, handle: GpuSceneDeformationHandle) -> bool {
        self.shared
            .prototypes
            .iter()
            .any(|(_, prototype)| prototype.deformation == Some(handle))
            || self
                .worlds
                .values()
                .flat_map(|world| world.instances.iter())
                .any(|(_, instance)| instance.deformation == Some(handle))
    }

    pub(super) fn sdf_is_referenced(&self, handle: GpuSceneSdfHandle) -> bool {
        self.shared
            .prototypes
            .iter()
            .any(|(_, prototype)| prototype.sdfs.contains(&handle))
    }

    pub(super) fn page_is_referenced(&self, handle: GpuScenePageHandle) -> bool {
        self.shared
            .prototypes
            .iter()
            .any(|(_, prototype)| prototype.root_page == handle)
            || self
                .shared
                .pages
                .iter()
                .any(|(_, page)| page.parent == Some(handle))
    }

    pub(super) fn validate_all_references(&self) -> Result<(), GpuSceneError> {
        for (_, material) in self.shared.materials.iter() {
            validate_device_handle(material.table, "material")?;
        }
        for (_, deformation) in self.shared.deformations.iter() {
            validate_device_handle(deformation.provider, "deformation")?;
        }
        for (_, sdf) in self.shared.sdfs.iter() {
            validate_device_handle(sdf.resource, "SDF")?;
        }
        for (_, page) in self.shared.pages.iter() {
            self.validate_page(None, page)?;
        }
        for (_, prototype) in self.shared.prototypes.iter() {
            self.validate_prototype(prototype)?;
        }
        for world in self.worlds.values() {
            for (_, instance) in world.instances.iter() {
                self.validate_instance(instance)?;
            }
            for (_, light) in world.lights.iter() {
                validate_light(light)?;
            }
        }
        Ok(())
    }
}

pub(super) fn validate_device_handle(
    handle: GpuHandle,
    kind: &'static str,
) -> Result<(), GpuSceneError> {
    if handle == GpuHandle::INVALID || handle.generation == 0 {
        return Err(GpuSceneError::StaleHandle { kind, handle });
    }
    Ok(())
}

pub(super) fn require<'a, T, K>(
    table: &'a SceneTable<T, K>,
    handle: GpuSceneHandle<K>,
    kind: &'static str,
) -> Result<&'a T, GpuSceneError> {
    table.get(handle).ok_or(GpuSceneError::StaleHandle {
        kind,
        handle: handle.raw(),
    })
}

pub(super) fn referenced(kind: &'static str, handle: GpuHandle) -> GpuSceneError {
    GpuSceneError::ReferencedHandle { kind, handle }
}

pub(super) fn validate_light(record: &GpuSceneLightRecord) -> Result<(), GpuSceneError> {
    let finite = record
        .light
        .position_range
        .to_array()
        .into_iter()
        .chain(record.light.color_intensity.to_array())
        .chain(record.light.direction_type.to_array())
        .chain(record.light.spot_cos.to_array())
        .all(f32::is_finite);
    if !finite {
        return Err(GpuSceneError::NonFinite("light"));
    }
    Ok(())
}
