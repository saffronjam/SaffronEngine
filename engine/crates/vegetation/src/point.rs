//! The schema-hashed canonical vegetation point vocabulary.

use std::collections::BTreeSet;

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, SignedUnit, SurfaceAttachment, UnitInterval, WorldBounds, WorldCellKey,
    WorldPosition,
};

use crate::hash::sha256;
use crate::{Error, PlantId, Result};

/// A registered point-column identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PointColumnId(pub u32);

/// The packed element shape of a point column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PointColumnType {
    /// Opaque 128-bit identifier.
    Id128 = 1,
    /// Hierarchical cell key.
    WorldCell = 2,
    /// Four signed normalized quaternion lanes.
    Orientation = 4,
    /// Three Q15.16 fixed scalars.
    FixedVec3 = 5,
    /// Half-open world bounds.
    WorldBounds = 6,
    /// Stable 64-bit asset identity.
    AssetUuid = 7,
    /// Unsigned 32-bit scalar.
    U32 = 8,
    /// Unsigned 64-bit scalar.
    U64 = 9,
    /// Optional opaque 128-bit identifier.
    OptionalId128 = 10,
    /// Closed normalized unsigned scalar.
    Unit = 11,
    /// Three Q15.16 projection coordinates.
    SurfaceProjection = 12,
    /// Optional stable surface attachment.
    OptionalSurfaceAttachment = 13,
    /// Exact level-zero cell plus three unsigned cell-local ticks.
    WorldPosition = 14,
}

/// One fixed column in the canonical point schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointColumnDescriptor {
    /// Stable numeric column identity.
    pub id: PointColumnId,
    /// Canonical semantic name.
    pub name: &'static str,
    /// Packed element shape.
    pub element_type: PointColumnType,
}

/// Fixed canonical schema, in packed serialization order.
pub const POINT_SCHEMA_COLUMNS: &[PointColumnDescriptor] = &[
    column(1, "plant-id", PointColumnType::Id128),
    column(2, "owner-cell", PointColumnType::WorldCell),
    column(3, "world-position", PointColumnType::WorldPosition),
    column(4, "orientation", PointColumnType::Orientation),
    column(5, "scale", PointColumnType::FixedVec3),
    column(6, "bounds", PointColumnType::WorldBounds),
    column(7, "family", PointColumnType::AssetUuid),
    column(8, "variation", PointColumnType::U32),
    column(9, "lifecycle", PointColumnType::U32),
    column(10, "phenotype", PointColumnType::U32),
    column(11, "representation-class", PointColumnType::U32),
    column(12, "deterministic-key", PointColumnType::Id128),
    column(13, "candidate", PointColumnType::U64),
    column(14, "parent", PointColumnType::OptionalId128),
    column(15, "colony", PointColumnType::OptionalId128),
    column(16, "ecology-tick", PointColumnType::U64),
    column(17, "health", PointColumnType::Unit),
    column(18, "moisture", PointColumnType::Unit),
    column(19, "fuel", PointColumnType::Unit),
    column(20, "phenology", PointColumnType::Unit),
    column(21, "flags", PointColumnType::U32),
    column(22, "interaction-policy", PointColumnType::U32),
    column(23, "provenance", PointColumnType::U32),
    column(
        24,
        "surface-attachment",
        PointColumnType::OptionalSurfaceAttachment,
    ),
    column(25, "surface-projection", PointColumnType::SurfaceProjection),
];

const fn column(
    id: u32,
    name: &'static str,
    element_type: PointColumnType,
) -> PointColumnDescriptor {
    PointColumnDescriptor {
        id: PointColumnId(id),
        name,
        element_type,
    }
}

/// The SHA-256 identity of the fixed point schema.
#[must_use]
pub fn point_schema_hash() -> [u8; 32] {
    let mut bytes = b"saffron-anima/vegetation-point-schema/v1\0".to_vec();
    for column in POINT_SCHEMA_COLUMNS {
        bytes.extend_from_slice(&column.id.0.to_be_bytes());
        bytes.push(column.element_type as u8);
        bytes.extend_from_slice(&(column.name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(column.name.as_bytes());
    }
    sha256(&bytes)
}

/// A quantized unit quaternion in canonical XYZW order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuantizedOrientation([SignedUnit; 4]);

impl QuantizedOrientation {
    /// Identity orientation.
    pub fn identity() -> Self {
        Self([
            SignedUnit::from_bits(0).unwrap(),
            SignedUnit::from_bits(0).unwrap(),
            SignedUnit::from_bits(0).unwrap(),
            SignedUnit::from_bits(i16::MAX).unwrap(),
        ])
    }

    /// Constructs a non-zero normalized quaternion within quantization tolerance.
    pub fn new(bits: [i16; 4]) -> Result<Self> {
        let lanes = bits
            .map(SignedUnit::from_bits)
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let length_squared: i64 = bits
            .into_iter()
            .map(|value| i64::from(value) * i64::from(value))
            .sum();
        let unit = i64::from(i16::MAX) * i64::from(i16::MAX);
        let tolerance = unit / 512;
        if length_squared.abs_diff(unit) > tolerance as u64 {
            return Err(Error::PointSchema(
                "orientation is not a quantized unit quaternion".to_owned(),
            ));
        }
        Ok(Self(lanes.try_into().unwrap()))
    }

    /// Canonical signed normalized lane bits.
    #[must_use]
    pub fn bits(self) -> [i16; 4] {
        self.0.map(SignedUnit::bits)
    }
}

impl Default for QuantizedOrientation {
    fn default() -> Self {
        Self::identity()
    }
}

/// Authored/runtime flags carried by every macro point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PlantFlags(u32);

impl PlantFlags {
    /// Explicit authored point rather than cooked procedural acceptance.
    pub const AUTHORED: Self = Self(1 << 0);
    /// Runtime-created point.
    pub const RUNTIME: Self = Self(1 << 1);
    /// A pin protects the point across graph recooks.
    pub const PINNED: Self = Self(1 << 2);
    /// A persistent transform override is active.
    pub const TRANSFORM_OVERRIDE: Self = Self(1 << 3);
    /// A persistent lifecycle/state override is active.
    pub const STATE_OVERRIDE: Self = Self(1 << 4);

    /// Constructs the exact packed bitset, rejecting unknown bits.
    pub fn from_bits(bits: u32) -> Result<Self> {
        const KNOWN: u32 = PlantFlags::AUTHORED.0
            | PlantFlags::RUNTIME.0
            | PlantFlags::PINNED.0
            | PlantFlags::TRANSFORM_OVERRIDE.0
            | PlantFlags::STATE_OVERRIDE.0;
        if bits & !KNOWN != 0 {
            return Err(Error::PointSchema("unknown plant flag bit".to_owned()));
        }
        Ok(Self(bits))
    }

    /// Packed flag bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns the union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Default gameplay interaction policy for one accepted plant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum InteractionPolicy {
    /// Decorative only; no promoted physics or gameplay state.
    #[default]
    Decorative = 0,
    /// Queryable and damageable macro plant.
    Interactive = 1,
    /// Can be harvested into products.
    Harvestable = 2,
    /// Provides collision/navigation obstruction when its facets are resident.
    Structural = 3,
}

impl TryFrom<u32> for InteractionPolicy {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Decorative),
            1 => Ok(Self::Interactive),
            2 => Ok(Self::Harvestable),
            3 => Ok(Self::Structural),
            _ => Err(Error::PointSchema("unknown interaction policy".to_owned())),
        }
    }
}

/// Complete canonical row before conversion to the columnar cell representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantPoint {
    /// Stable opaque plant identity.
    pub id: PlantId,
    /// Canonical storage owner.
    pub owner: WorldCellKey,
    /// Exact world position; its local ticks populate the local-position column.
    pub position: WorldPosition,
    /// Quantized XYZW orientation.
    pub orientation: QuantizedOrientation,
    /// Q15.16 local scale.
    pub scale: [DecisionScalar; 3],
    /// Conservative half-open bounds.
    pub bounds: WorldBounds,
    /// Plant-family asset.
    pub family: Uuid,
    /// Family variation.
    pub variation: u32,
    /// Typed lifecycle state.
    pub lifecycle: PlantLifecycle,
    /// Species-declared phenotype/life-state variant.
    pub phenotype: u32,
    /// Renderer-independent representation class.
    pub representation_class: u32,
    /// Stable deterministic candidate key.
    pub deterministic_key: u128,
    /// Candidate ordinal in the sampler namespace.
    pub candidate: u64,
    /// Optional parent plant.
    pub parent: Option<PlantId>,
    /// Optional colony/root plant.
    pub colony: Option<PlantId>,
    /// Monotonic biological age tick.
    pub ecology_tick: u64,
    /// Persistent health.
    pub health: UnitInterval,
    /// Persistent moisture.
    pub moisture: UnitInterval,
    /// Persistent fuel.
    pub fuel: UnitInterval,
    /// Current phenotype/calendar phase.
    pub phenology: UnitInterval,
    /// Authored/runtime flags.
    pub flags: PlantFlags,
    /// Gameplay interaction policy.
    pub interaction_policy: InteractionPolicy,
    /// Compact provenance-table handle.
    pub provenance: u32,
    /// Stable attachment when a surface owns the plant.
    pub attachment: Option<SurfaceAttachment>,
    /// Provider-local projection coordinates, quantized for authority.
    pub surface_projection: [DecisionScalar; 3],
}

impl PlantPoint {
    /// Validates ownership, bounds, scale, and identity invariants.
    pub fn validate(&self) -> Result<()> {
        self.id.namespace()?;
        if !self.owner.bounds().contains(self.position) {
            return Err(Error::PointSchema(
                "owner cell does not contain point position".to_owned(),
            ));
        }
        if !self.bounds.contains(self.position) {
            return Err(Error::PointSchema(
                "conservative bounds do not contain point position".to_owned(),
            ));
        }
        if self.scale.iter().any(|scale| scale.bits() <= 0) {
            return Err(Error::PointSchema(
                "plant scale must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Canonical biological lifecycle state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum PlantLifecycle {
    /// Dormant seed.
    #[default]
    Seed = 0,
    /// Emerged sprout.
    Sprout = 1,
    /// Growing juvenile.
    Juvenile = 2,
    /// Mature plant.
    Mature = 3,
    /// Senescent plant.
    Senescent = 4,
    /// Dead standing/fallen plant.
    Dead = 5,
    /// Remaining stump/root structure.
    Stump = 6,
    /// Persistently removed/tombstoned.
    Removed = 7,
}

impl TryFrom<u32> for PlantLifecycle {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Seed),
            1 => Ok(Self::Sprout),
            2 => Ok(Self::Juvenile),
            3 => Ok(Self::Mature),
            4 => Ok(Self::Senescent),
            5 => Ok(Self::Dead),
            6 => Ok(Self::Stump),
            7 => Ok(Self::Removed),
            _ => Err(Error::PointSchema("unknown lifecycle state".to_owned())),
        }
    }
}

/// One registered extension column. IDs below `0x8000_0000` are reserved by the fixed schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionColumn {
    /// Registered extension ID.
    pub id: PointColumnId,
    /// Packed element type.
    pub element_type: PointColumnType,
    /// Packed bytes per row.
    pub stride: u32,
    /// Concatenated canonical row bytes.
    pub bytes: Vec<u8>,
}

/// Structure-of-arrays canonical CPU point table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlantPointColumns {
    /// Stable identities.
    pub ids: Vec<PlantId>,
    /// Canonical owner cells.
    pub owner_cells: Vec<WorldCellKey>,
    /// Exact quantized world positions, independent of logical hierarchy ownership.
    pub positions: Vec<WorldPosition>,
    /// Quantized orientations.
    pub orientations: Vec<QuantizedOrientation>,
    /// Fixed scales.
    pub scales: Vec<[DecisionScalar; 3]>,
    /// Conservative world bounds.
    pub bounds: Vec<WorldBounds>,
    /// Family assets.
    pub families: Vec<Uuid>,
    /// Family variations.
    pub variations: Vec<u32>,
    /// Lifecycle states.
    pub lifecycles: Vec<PlantLifecycle>,
    /// Phenotypes.
    pub phenotypes: Vec<u32>,
    /// Representation classes.
    pub representation_classes: Vec<u32>,
    /// Stable candidate keys.
    pub deterministic_keys: Vec<u128>,
    /// Candidate ordinals.
    pub candidates: Vec<u64>,
    /// Parent identities.
    pub parents: Vec<Option<PlantId>>,
    /// Colony identities.
    pub colonies: Vec<Option<PlantId>>,
    /// Biological ticks.
    pub ecology_ticks: Vec<u64>,
    /// Health values.
    pub health: Vec<UnitInterval>,
    /// Moisture values.
    pub moisture: Vec<UnitInterval>,
    /// Fuel values.
    pub fuel: Vec<UnitInterval>,
    /// Phenology values.
    pub phenology: Vec<UnitInterval>,
    /// Flag sets.
    pub flags: Vec<PlantFlags>,
    /// Interaction policies.
    pub interaction_policies: Vec<InteractionPolicy>,
    /// Provenance handles.
    pub provenance: Vec<u32>,
    /// Surface attachments.
    pub attachments: Vec<Option<SurfaceAttachment>>,
    /// Surface projection coordinates.
    pub surface_projections: Vec<[DecisionScalar; 3]>,
    /// Registered packed extension columns.
    pub extensions: Vec<ExtensionColumn>,
}

impl PlantPointColumns {
    /// Converts validated rows to the canonical structure-of-arrays table.
    pub fn from_points(points: &[PlantPoint]) -> Result<Self> {
        let mut columns = Self::default();
        let mut ids = BTreeSet::new();
        for point in points {
            point.validate()?;
            if !ids.insert(point.id) {
                return Err(Error::DuplicatePlantId(point.id.to_string()));
            }
            columns.ids.push(point.id);
            columns.owner_cells.push(point.owner);
            columns.positions.push(point.position);
            columns.orientations.push(point.orientation);
            columns.scales.push(point.scale);
            columns.bounds.push(point.bounds);
            columns.families.push(point.family);
            columns.variations.push(point.variation);
            columns.lifecycles.push(point.lifecycle);
            columns.phenotypes.push(point.phenotype);
            columns
                .representation_classes
                .push(point.representation_class);
            columns.deterministic_keys.push(point.deterministic_key);
            columns.candidates.push(point.candidate);
            columns.parents.push(point.parent);
            columns.colonies.push(point.colony);
            columns.ecology_ticks.push(point.ecology_tick);
            columns.health.push(point.health);
            columns.moisture.push(point.moisture);
            columns.fuel.push(point.fuel);
            columns.phenology.push(point.phenology);
            columns.flags.push(point.flags);
            columns.interaction_policies.push(point.interaction_policy);
            columns.provenance.push(point.provenance);
            columns.attachments.push(point.attachment);
            columns.surface_projections.push(point.surface_projection);
        }
        columns.validate()?;
        Ok(columns)
    }

    /// Number of point rows after validating every column length.
    pub fn row_count(&self) -> Result<usize> {
        self.validate()?;
        Ok(self.ids.len())
    }

    /// Adds one registered packed extension column.
    pub fn add_extension(&mut self, column: ExtensionColumn) -> Result<()> {
        if column.id.0 < 0x8000_0000 || column.stride == 0 {
            return Err(Error::PointSchema(
                "extension column id/stride is invalid".to_owned(),
            ));
        }
        if self
            .extensions
            .iter()
            .any(|existing| existing.id == column.id)
        {
            return Err(Error::PointSchema(
                "duplicate extension column id".to_owned(),
            ));
        }
        let expected = self
            .ids
            .len()
            .checked_mul(column.stride as usize)
            .ok_or(Error::NumericOverflow)?;
        if column.bytes.len() != expected {
            return Err(Error::PointSchema(
                "extension column byte length does not match row count".to_owned(),
            ));
        }
        self.extensions.push(column);
        self.extensions.sort_by_key(|column| column.id);
        Ok(())
    }

    /// Validates fixed/extension column lengths and identity uniqueness.
    pub fn validate(&self) -> Result<()> {
        let rows = self.ids.len();
        let lengths = [
            self.owner_cells.len(),
            self.positions.len(),
            self.orientations.len(),
            self.scales.len(),
            self.bounds.len(),
            self.families.len(),
            self.variations.len(),
            self.lifecycles.len(),
            self.phenotypes.len(),
            self.representation_classes.len(),
            self.deterministic_keys.len(),
            self.candidates.len(),
            self.parents.len(),
            self.colonies.len(),
            self.ecology_ticks.len(),
            self.health.len(),
            self.moisture.len(),
            self.fuel.len(),
            self.phenology.len(),
            self.flags.len(),
            self.interaction_policies.len(),
            self.provenance.len(),
            self.attachments.len(),
            self.surface_projections.len(),
        ];
        if lengths.into_iter().any(|length| length != rows) {
            return Err(Error::PointSchema(
                "fixed point columns have different row counts".to_owned(),
            ));
        }
        let unique: BTreeSet<PlantId> = self.ids.iter().copied().collect();
        if unique.len() != rows {
            return Err(Error::PointSchema(
                "point identity column contains duplicates".to_owned(),
            ));
        }
        let mut extension_ids = BTreeSet::new();
        for column in &self.extensions {
            if column.id.0 < 0x8000_0000
                || column.stride == 0
                || !extension_ids.insert(column.id)
                || column.bytes.len() != rows.saturating_mul(column.stride as usize)
            {
                return Err(Error::PointSchema(
                    "extension column violates registry/packing rules".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Canonical big-endian packed bytes, used by cooks, manifests, and equality tests.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"SVEGPT01");
        bytes.extend_from_slice(&point_schema_hash());
        bytes.extend_from_slice(&(self.ids.len() as u64).to_be_bytes());
        for row in 0..self.ids.len() {
            bytes.extend_from_slice(&self.ids[row].bytes());
            bytes.extend_from_slice(&self.owner_cells[row].canonical_bytes());
            bytes.extend_from_slice(&self.positions[row].cell().canonical_bytes());
            for tick in self.positions[row].local().ticks() {
                bytes.extend_from_slice(&tick.to_be_bytes());
            }
            for lane in self.orientations[row].bits() {
                bytes.extend_from_slice(&lane.to_be_bytes());
            }
            for scale in self.scales[row] {
                bytes.extend_from_slice(&scale.canonical_bytes());
            }
            for tick in self.bounds[row].min_ticks() {
                bytes.extend_from_slice(&tick.to_be_bytes());
            }
            for tick in self.bounds[row].max_ticks_exclusive() {
                bytes.extend_from_slice(&tick.to_be_bytes());
            }
            bytes.extend_from_slice(&self.families[row].value().to_be_bytes());
            bytes.extend_from_slice(&self.variations[row].to_be_bytes());
            bytes.extend_from_slice(&(self.lifecycles[row] as u32).to_be_bytes());
            bytes.extend_from_slice(&self.phenotypes[row].to_be_bytes());
            bytes.extend_from_slice(&self.representation_classes[row].to_be_bytes());
            bytes.extend_from_slice(&self.deterministic_keys[row].to_be_bytes());
            bytes.extend_from_slice(&self.candidates[row].to_be_bytes());
            push_optional_id(&mut bytes, self.parents[row]);
            push_optional_id(&mut bytes, self.colonies[row]);
            bytes.extend_from_slice(&self.ecology_ticks[row].to_be_bytes());
            bytes.extend_from_slice(&self.health[row].canonical_bytes());
            bytes.extend_from_slice(&self.moisture[row].canonical_bytes());
            bytes.extend_from_slice(&self.fuel[row].canonical_bytes());
            bytes.extend_from_slice(&self.phenology[row].canonical_bytes());
            bytes.extend_from_slice(&self.flags[row].bits().to_be_bytes());
            bytes.extend_from_slice(&(self.interaction_policies[row] as u32).to_be_bytes());
            bytes.extend_from_slice(&self.provenance[row].to_be_bytes());
            push_attachment(&mut bytes, self.attachments[row]);
            for coordinate in self.surface_projections[row] {
                bytes.extend_from_slice(&coordinate.canonical_bytes());
            }
        }
        bytes.extend_from_slice(&(self.extensions.len() as u32).to_be_bytes());
        for column in &self.extensions {
            bytes.extend_from_slice(&column.id.0.to_be_bytes());
            bytes.push(column.element_type as u8);
            bytes.extend_from_slice(&column.stride.to_be_bytes());
            bytes.extend_from_slice(&(column.bytes.len() as u64).to_be_bytes());
            bytes.extend_from_slice(&column.bytes);
        }
        Ok(bytes)
    }
}

fn push_optional_id(bytes: &mut Vec<u8>, id: Option<PlantId>) {
    match id {
        Some(id) => {
            bytes.push(1);
            bytes.extend_from_slice(&id.bytes());
        }
        None => bytes.push(0),
    }
}

fn push_attachment(bytes: &mut Vec<u8>, attachment: Option<SurfaceAttachment>) {
    match attachment {
        Some(attachment) => {
            bytes.push(1);
            bytes.extend_from_slice(&attachment.provider.0.to_be_bytes());
            bytes.extend_from_slice(&attachment.primitive.0.to_be_bytes());
            for weight in attachment.barycentric {
                bytes.extend_from_slice(&weight.canonical_bytes());
            }
            bytes.extend_from_slice(&attachment.revision.0.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

#[cfg(test)]
mod tests {
    use saffron_spatial::{DecisionScalar, WorldBounds};

    use super::*;
    use crate::{PlantId, ProceduralPlantIdentity};

    fn point(candidate: u64) -> PlantPoint {
        let position = WorldPosition::from_global_ticks([candidate as i128, 0, 0]).unwrap();
        PlantPoint {
            id: PlantId::procedural(ProceduralPlantIdentity {
                map: Uuid(1),
                layer_guid: 2,
                node_address: 3,
                node_semantic_revision: 4,
                candidate,
                ancestor: 0,
                seed_namespace: 5,
                owner: position.cell(),
                family: Uuid(6),
            }),
            owner: position.cell(),
            position,
            orientation: QuantizedOrientation::identity(),
            scale: [DecisionScalar::from_integer(1).unwrap(); 3],
            bounds: WorldBounds::new(
                [candidate as i128 - 1, -1, -1],
                [candidate as i128 + 2, 2, 2],
            )
            .unwrap(),
            family: Uuid(6),
            variation: 0,
            lifecycle: PlantLifecycle::Mature,
            phenotype: 0,
            representation_class: 0,
            deterministic_key: u128::from(candidate),
            candidate,
            parent: None,
            colony: None,
            ecology_tick: 10,
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(30_000),
            fuel: UnitInterval::from_bits(20_000),
            phenology: UnitInterval::from_bits(10_000),
            flags: PlantFlags::default(),
            interaction_policy: InteractionPolicy::Interactive,
            provenance: 7,
            attachment: None,
            surface_projection: [DecisionScalar::from_bits(0); 3],
        }
    }

    #[test]
    fn point_schema_hash_is_pinned() {
        assert_eq!(
            point_schema_hash(),
            [
                0x35, 0x52, 0x30, 0xd3, 0xbb, 0xa6, 0x18, 0x24, 0x8e, 0xb4, 0xb2, 0xfb, 0xa4, 0xee,
                0x31, 0x0b, 0x5e, 0x5b, 0x1b, 0x33, 0x36, 0xcc, 0x68, 0x64, 0xd4, 0xd2, 0x73, 0x56,
                0xfd, 0x4d, 0x02, 0xb8,
            ]
        );
    }

    #[test]
    fn columns_are_canonical_and_schema_hashed() {
        let a = PlantPointColumns::from_points(&[point(1), point(2)]).unwrap();
        let b = PlantPointColumns::from_points(&[point(1), point(2)]).unwrap();
        assert_eq!(a.canonical_bytes().unwrap(), b.canonical_bytes().unwrap());
        assert_eq!(&a.canonical_bytes().unwrap()[8..40], &point_schema_hash());
    }

    #[test]
    fn extensions_are_registered_sorted_and_row_sized() {
        let mut columns = PlantPointColumns::from_points(&[point(1), point(2)]).unwrap();
        columns
            .add_extension(ExtensionColumn {
                id: PointColumnId(0x8000_0002),
                element_type: PointColumnType::U32,
                stride: 4,
                bytes: vec![0; 8],
            })
            .unwrap();
        columns
            .add_extension(ExtensionColumn {
                id: PointColumnId(0x8000_0001),
                element_type: PointColumnType::U32,
                stride: 4,
                bytes: vec![1; 8],
            })
            .unwrap();
        assert_eq!(columns.extensions[0].id, PointColumnId(0x8000_0001));
        assert!(
            columns
                .add_extension(ExtensionColumn {
                    id: PointColumnId(99),
                    element_type: PointColumnType::U32,
                    stride: 4,
                    bytes: vec![0; 8],
                })
                .is_err()
        );
    }

    #[test]
    fn duplicate_identity_is_rejected_before_publication() {
        let duplicate = point(1);
        assert!(PlantPointColumns::from_points(&[duplicate.clone(), duplicate]).is_err());
    }
}
