//! The schema-hashed canonical vegetation point vocabulary.

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, QuantizedLocalPosition, QuantizedOrientation, SurfaceAttachment,
    SurfacePrimitiveId, SurfaceProviderId, SurfaceRevision, UnitInterval, WorldBounds,
    WorldCellKey, WorldPosition,
};

use crate::binary::BinaryReader;
use crate::canonical::{ByteSink, CanonicalSink, CountSink};
use crate::hash::VegetationContentHasher;
use crate::memory::{
    checked_memory_sum, requested_vec_bytes, requested_vec_bytes_for_len, requested_vec_with,
    reserve_exact,
};
use crate::{Error, PlantId, Result};

/// A registered point-column identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PointColumnId(pub u32);

/// The packed element shape of a point column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PointColumnType {
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
    U32 = 8,
    U64 = 9,
    OptionalId128 = 10,
    /// Closed normalized unsigned scalar.
    Unit = 11,
    /// Three Q15.16 projection coordinates.
    SurfaceProjection = 12,
    OptionalSurfaceAttachment = 13,
    /// Exact level-zero cell plus three unsigned cell-local ticks.
    WorldPosition = 14,
}

/// One fixed column in the canonical point schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointColumnDescriptor {
    /// Stable numeric column identity.
    pub id: PointColumnId,
    pub name: &'static str,
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
    let mut hasher = VegetationContentHasher::new();
    hasher
        .update(b"saffron-anima/vegetation-point-schema/v1\0")
        .expect("the fixed point schema fits the SHA-256 message bound");
    for column in POINT_SCHEMA_COLUMNS {
        hasher
            .update(&column.id.0.to_be_bytes())
            .and_then(|()| hasher.update(&[column.element_type as u8]))
            .and_then(|()| hasher.update(&(column.name.len() as u16).to_be_bytes()))
            .and_then(|()| hasher.update(column.name.as_bytes()))
            .expect("the fixed point schema fits the SHA-256 message bound");
    }
    hasher
        .finalize()
        .expect("the fixed point schema fits the SHA-256 message bound")
}

/// Authored/runtime flags carried by every macro point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PlantFlags(u32);

impl PlantFlags {
    /// Explicit authored point rather than cooked procedural acceptance.
    pub const AUTHORED: Self = Self(1 << 0);
    pub const RUNTIME: Self = Self(1 << 1);
    /// A pin protects the point across graph recooks.
    pub const PINNED: Self = Self(1 << 2);
    /// A persistent transform override is active.
    pub const TRANSFORM_OVERRIDE: Self = Self(1 << 3);
    /// A persistent lifecycle/state override is active.
    pub const STATE_OVERRIDE: Self = Self(1 << 4);
    pub const IGNITED: Self = Self(1 << 5);

    /// Constructs the exact packed bitset, rejecting unknown bits.
    pub fn from_bits(bits: u32) -> Result<Self> {
        const KNOWN: u32 = PlantFlags::AUTHORED.0
            | PlantFlags::RUNTIME.0
            | PlantFlags::PINNED.0
            | PlantFlags::TRANSFORM_OVERRIDE.0
            | PlantFlags::STATE_OVERRIDE.0
            | PlantFlags::IGNITED.0;
        if bits & !KNOWN != 0 {
            return Err(Error::PointSchema("unknown plant flag bit".to_owned()));
        }
        Ok(Self(bits))
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns the flag set without `other`.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
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
    pub variation: u32,
    pub lifecycle: PlantLifecycle,
    /// Species-declared phenotype/life-state variant.
    pub phenotype: u32,
    /// Renderer-independent representation class.
    pub representation_class: u32,
    /// Stable deterministic candidate key.
    pub deterministic_key: u128,
    /// Candidate ordinal in the sampler namespace.
    pub candidate: u64,
    pub parent: Option<PlantId>,
    /// Colony or root plant this one belongs to.
    pub colony: Option<PlantId>,
    /// Monotonic biological age tick.
    pub ecology_tick: u64,
    pub health: UnitInterval,
    pub moisture: UnitInterval,
    pub fuel: UnitInterval,
    /// Current phenotype/calendar phase.
    pub phenology: UnitInterval,
    pub flags: PlantFlags,
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
    #[default]
    Seed = 0,
    Sprout = 1,
    Juvenile = 2,
    Mature = 3,
    Senescent = 4,
    /// Dead but still standing or fallen.
    Dead = 5,
    /// Remaining stump/root structure.
    Stump = 6,
    /// Persistently removed and tombstoned.
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
    pub id: PointColumnId,
    pub element_type: PointColumnType,
    /// Packed bytes per row.
    pub stride: u32,
    /// Concatenated canonical row bytes.
    pub bytes: Vec<u8>,
}

/// One extension-column value projected for a single macro-point row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantPointExtensionValue<'a> {
    pub id: PointColumnId,
    pub element_type: PointColumnType,
    /// Exact canonical bytes for this row.
    pub bytes: &'a [u8],
}

/// One complete macro-point row with its registered extension values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantPointRow<'a> {
    pub point: PlantPoint,
    /// Extension values in canonical column-ID order.
    pub extensions: Vec<PlantPointExtensionValue<'a>>,
}

/// Structure-of-arrays canonical CPU point table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlantPointColumns {
    pub ids: Vec<PlantId>,
    pub owner_cells: Vec<WorldCellKey>,
    /// Exact quantized world positions, independent of logical hierarchy ownership.
    pub positions: Vec<WorldPosition>,
    pub orientations: Vec<QuantizedOrientation>,
    pub scales: Vec<[DecisionScalar; 3]>,
    pub bounds: Vec<WorldBounds>,
    pub families: Vec<Uuid>,
    pub variations: Vec<u32>,
    pub lifecycles: Vec<PlantLifecycle>,
    pub phenotypes: Vec<u32>,
    pub representation_classes: Vec<u32>,
    pub deterministic_keys: Vec<u128>,
    pub candidates: Vec<u64>,
    pub parents: Vec<Option<PlantId>>,
    pub colonies: Vec<Option<PlantId>>,
    pub ecology_ticks: Vec<u64>,
    pub health: Vec<UnitInterval>,
    pub moisture: Vec<UnitInterval>,
    pub fuel: Vec<UnitInterval>,
    pub phenology: Vec<UnitInterval>,
    pub flags: Vec<PlantFlags>,
    pub interaction_policies: Vec<InteractionPolicy>,
    pub provenance: Vec<u32>,
    pub attachments: Vec<Option<SurfaceAttachment>>,
    pub surface_projections: Vec<[DecisionScalar; 3]>,
    /// Registered packed extension columns, in column-ID order.
    pub extensions: Vec<ExtensionColumn>,
}

impl PlantPointColumns {
    /// Decodes the one canonical macro-point payload accepted by this build.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = BinaryReader::new(bytes, "vegetation macro points");
        reader.expect(b"SVEGPT01", "magic")?;
        reader.expect(&point_schema_hash(), "schemaHash")?;
        let rows = reader.count(285)?;
        let mut columns = Self::default();
        columns.reserve_rows(rows)?;
        for _ in 0..rows {
            columns.ids.push(PlantId::from_bytes(reader.array()?)?);
            columns.owner_cells.push(reader.cell()?);
            let position_cell = reader.cell()?;
            let local = QuantizedLocalPosition::new([reader.u32()?, reader.u32()?, reader.u32()?])?;
            columns
                .positions
                .push(WorldPosition::new(position_cell, local)?);
            columns.orientations.push(QuantizedOrientation::new([
                reader.u16()? as i16,
                reader.u16()? as i16,
                reader.u16()? as i16,
                reader.u16()? as i16,
            ])?);
            columns.scales.push([
                DecisionScalar::from_bits(reader.i32()?),
                DecisionScalar::from_bits(reader.i32()?),
                DecisionScalar::from_bits(reader.i32()?),
            ]);
            columns.bounds.push(reader.bounds()?);
            columns.families.push(reader.uuid()?);
            columns.variations.push(reader.u32()?);
            columns
                .lifecycles
                .push(PlantLifecycle::try_from(reader.u32()?)?);
            columns.phenotypes.push(reader.u32()?);
            columns.representation_classes.push(reader.u32()?);
            columns.deterministic_keys.push(reader.u128()?);
            columns.candidates.push(reader.u64()?);
            columns.parents.push(read_optional_plant_id(&mut reader)?);
            columns.colonies.push(read_optional_plant_id(&mut reader)?);
            columns.ecology_ticks.push(reader.u64()?);
            columns.health.push(UnitInterval::from_bits(reader.u16()?));
            columns
                .moisture
                .push(UnitInterval::from_bits(reader.u16()?));
            columns.fuel.push(UnitInterval::from_bits(reader.u16()?));
            columns
                .phenology
                .push(UnitInterval::from_bits(reader.u16()?));
            columns.flags.push(PlantFlags::from_bits(reader.u32()?)?);
            columns
                .interaction_policies
                .push(InteractionPolicy::try_from(reader.u32()?)?);
            columns.provenance.push(reader.u32()?);
            columns.attachments.push(read_attachment(&mut reader)?);
            columns.surface_projections.push([
                DecisionScalar::from_bits(reader.i32()?),
                DecisionScalar::from_bits(reader.i32()?),
                DecisionScalar::from_bits(reader.i32()?),
            ]);
        }
        let extension_count = usize::try_from(reader.u32()?).map_err(|_| Error::NumericOverflow)?;
        if extension_count > reader.remaining() / 17 {
            return Err(Error::ArtifactTruncated {
                format: "vegetation macro points",
            });
        }
        columns
            .extensions
            .try_reserve_exact(extension_count)
            .map_err(|source| Error::MemoryReservation {
                resource: "decoded vegetation point extensions",
                source,
            })?;
        for _ in 0..extension_count {
            let id = PointColumnId(reader.u32()?);
            let element_type =
                point_column_type_from_id(reader.u8()?).ok_or_else(|| Error::ArtifactFormat {
                    format: "vegetation macro points",
                    field: "extension.elementType".to_owned(),
                })?;
            let stride = reader.u32()?;
            let byte_length = reader.length()?;
            columns.extensions.push(ExtensionColumn {
                id,
                element_type,
                stride,
                bytes: reader.take(byte_length)?.to_vec(),
            });
        }
        reader.complete()?;
        columns.validate()?;
        for row in 0..rows {
            columns.point_unchecked(row).validate()?;
        }
        Ok(columns)
    }

    /// Projects one validated column index into its complete canonical row.
    pub fn point(&self, row: usize) -> Result<PlantPoint> {
        self.validate()?;
        if row >= self.ids.len() {
            return Err(Error::PointSchema(
                "point row index is out of bounds".to_owned(),
            ));
        }
        let point = self.point_unchecked(row);
        point.validate()?;
        Ok(point)
    }

    /// Projects one validated column index including every registered extension value.
    pub fn row(&self, row: usize) -> Result<PlantPointRow<'_>> {
        let point = self.point(row)?;
        let mut extensions = Vec::new();
        reserve_exact(
            &mut extensions,
            self.extensions.len(),
            "projected plant point extensions",
        )?;
        for column in &self.extensions {
            let stride = usize::try_from(column.stride).map_err(|_| Error::NumericOverflow)?;
            let start = row.checked_mul(stride).ok_or(Error::NumericOverflow)?;
            let end = start.checked_add(stride).ok_or(Error::NumericOverflow)?;
            let bytes = column.bytes.get(start..end).ok_or_else(|| {
                Error::PointSchema("extension row range is out of bounds".to_owned())
            })?;
            extensions.push(PlantPointExtensionValue {
                id: column.id,
                element_type: column.element_type,
                bytes,
            });
        }
        Ok(PlantPointRow { point, extensions })
    }

    fn point_unchecked(&self, row: usize) -> PlantPoint {
        PlantPoint {
            id: self.ids[row],
            owner: self.owner_cells[row],
            position: self.positions[row],
            orientation: self.orientations[row],
            scale: self.scales[row],
            bounds: self.bounds[row],
            family: self.families[row],
            variation: self.variations[row],
            lifecycle: self.lifecycles[row],
            phenotype: self.phenotypes[row],
            representation_class: self.representation_classes[row],
            deterministic_key: self.deterministic_keys[row],
            candidate: self.candidates[row],
            parent: self.parents[row],
            colony: self.colonies[row],
            ecology_tick: self.ecology_ticks[row],
            health: self.health[row],
            moisture: self.moisture[row],
            fuel: self.fuel[row],
            phenology: self.phenology[row],
            flags: self.flags[row],
            interaction_policy: self.interaction_policies[row],
            provenance: self.provenance[row],
            attachment: self.attachments[row],
            surface_projection: self.surface_projections[row],
        }
    }

    /// Converts validated rows to the canonical structure-of-arrays table.
    pub fn from_points(mut points: Vec<PlantPoint>) -> Result<Self> {
        let mut columns = Self::default();
        points.sort_unstable_by_key(|point| point.id);
        for pair in points.windows(2) {
            if pair[0].id == pair[1].id {
                return Err(Error::DuplicatePlantId(pair[0].id.to_string()));
            }
        }
        columns.reserve_rows(points.len())?;
        for point in points {
            point.validate()?;
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

    pub(crate) fn reserve_rows(&mut self, rows: usize) -> Result<()> {
        reserve_exact(&mut self.ids, rows, "plant point ids")?;
        reserve_exact(&mut self.owner_cells, rows, "plant point owner cells")?;
        reserve_exact(&mut self.positions, rows, "plant point positions")?;
        reserve_exact(&mut self.orientations, rows, "plant point orientations")?;
        reserve_exact(&mut self.scales, rows, "plant point scales")?;
        reserve_exact(&mut self.bounds, rows, "plant point bounds")?;
        reserve_exact(&mut self.families, rows, "plant point families")?;
        reserve_exact(&mut self.variations, rows, "plant point variations")?;
        reserve_exact(&mut self.lifecycles, rows, "plant point lifecycles")?;
        reserve_exact(&mut self.phenotypes, rows, "plant point phenotypes")?;
        reserve_exact(
            &mut self.representation_classes,
            rows,
            "plant point representation classes",
        )?;
        reserve_exact(
            &mut self.deterministic_keys,
            rows,
            "plant point deterministic keys",
        )?;
        reserve_exact(&mut self.candidates, rows, "plant point candidates")?;
        reserve_exact(&mut self.parents, rows, "plant point parents")?;
        reserve_exact(&mut self.colonies, rows, "plant point colonies")?;
        reserve_exact(&mut self.ecology_ticks, rows, "plant point ecology ticks")?;
        reserve_exact(&mut self.health, rows, "plant point health")?;
        reserve_exact(&mut self.moisture, rows, "plant point moisture")?;
        reserve_exact(&mut self.fuel, rows, "plant point fuel")?;
        reserve_exact(&mut self.phenology, rows, "plant point phenology")?;
        reserve_exact(&mut self.flags, rows, "plant point flags")?;
        reserve_exact(
            &mut self.interaction_policies,
            rows,
            "plant point interaction policies",
        )?;
        reserve_exact(&mut self.provenance, rows, "plant point provenance")?;
        reserve_exact(&mut self.attachments, rows, "plant point attachments")?;
        reserve_exact(
            &mut self.surface_projections,
            rows,
            "plant point surface projections",
        )
    }

    pub(crate) fn requested_memory_bytes(&self) -> Result<u64> {
        checked_memory_sum([
            requested_vec_bytes::<PlantId>(self.ids.capacity())?,
            requested_vec_bytes::<WorldCellKey>(self.owner_cells.capacity())?,
            requested_vec_bytes::<WorldPosition>(self.positions.capacity())?,
            requested_vec_bytes::<QuantizedOrientation>(self.orientations.capacity())?,
            requested_vec_bytes::<[DecisionScalar; 3]>(self.scales.capacity())?,
            requested_vec_bytes::<WorldBounds>(self.bounds.capacity())?,
            requested_vec_bytes::<Uuid>(self.families.capacity())?,
            requested_vec_bytes::<u32>(self.variations.capacity())?,
            requested_vec_bytes::<PlantLifecycle>(self.lifecycles.capacity())?,
            requested_vec_bytes::<u32>(self.phenotypes.capacity())?,
            requested_vec_bytes::<u32>(self.representation_classes.capacity())?,
            requested_vec_bytes::<u128>(self.deterministic_keys.capacity())?,
            requested_vec_bytes::<u64>(self.candidates.capacity())?,
            requested_vec_bytes::<Option<PlantId>>(self.parents.capacity())?,
            requested_vec_bytes::<Option<PlantId>>(self.colonies.capacity())?,
            requested_vec_bytes::<u64>(self.ecology_ticks.capacity())?,
            requested_vec_bytes::<UnitInterval>(self.health.capacity())?,
            requested_vec_bytes::<UnitInterval>(self.moisture.capacity())?,
            requested_vec_bytes::<UnitInterval>(self.fuel.capacity())?,
            requested_vec_bytes::<UnitInterval>(self.phenology.capacity())?,
            requested_vec_bytes::<PlantFlags>(self.flags.capacity())?,
            requested_vec_bytes::<InteractionPolicy>(self.interaction_policies.capacity())?,
            requested_vec_bytes::<u32>(self.provenance.capacity())?,
            requested_vec_bytes::<Option<SurfaceAttachment>>(self.attachments.capacity())?,
            requested_vec_bytes::<[DecisionScalar; 3]>(self.surface_projections.capacity())?,
            requested_vec_with(&self.extensions, |column| {
                requested_vec_bytes::<u8>(column.bytes.capacity())
            })?,
        ])
    }

    pub(crate) fn requested_memory_bytes_for_rows(rows: u64) -> Result<u64> {
        checked_memory_sum([
            requested_vec_bytes_for_len::<PlantId>(rows)?,
            requested_vec_bytes_for_len::<WorldCellKey>(rows)?,
            requested_vec_bytes_for_len::<WorldPosition>(rows)?,
            requested_vec_bytes_for_len::<QuantizedOrientation>(rows)?,
            requested_vec_bytes_for_len::<[DecisionScalar; 3]>(rows)?,
            requested_vec_bytes_for_len::<WorldBounds>(rows)?,
            requested_vec_bytes_for_len::<Uuid>(rows)?,
            requested_vec_bytes_for_len::<u32>(rows)?,
            requested_vec_bytes_for_len::<PlantLifecycle>(rows)?,
            requested_vec_bytes_for_len::<u32>(rows)?,
            requested_vec_bytes_for_len::<u32>(rows)?,
            requested_vec_bytes_for_len::<u128>(rows)?,
            requested_vec_bytes_for_len::<u64>(rows)?,
            requested_vec_bytes_for_len::<Option<PlantId>>(rows)?,
            requested_vec_bytes_for_len::<Option<PlantId>>(rows)?,
            requested_vec_bytes_for_len::<u64>(rows)?,
            requested_vec_bytes_for_len::<UnitInterval>(rows)?,
            requested_vec_bytes_for_len::<UnitInterval>(rows)?,
            requested_vec_bytes_for_len::<UnitInterval>(rows)?,
            requested_vec_bytes_for_len::<UnitInterval>(rows)?,
            requested_vec_bytes_for_len::<PlantFlags>(rows)?,
            requested_vec_bytes_for_len::<InteractionPolicy>(rows)?,
            requested_vec_bytes_for_len::<u32>(rows)?,
            requested_vec_bytes_for_len::<Option<SurfaceAttachment>>(rows)?,
            requested_vec_bytes_for_len::<[DecisionScalar; 3]>(rows)?,
        ])
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
        self.extensions.sort_unstable_by_key(|column| column.id);
        Ok(())
    }

    /// Validates fixed/extension column lengths and canonical identity ordering.
    pub fn validate(&self) -> Result<()> {
        self.validate_guarded(|| Ok(()))
    }

    pub(crate) fn validate_guarded<F>(&self, mut guard: F) -> Result<()>
    where
        F: FnMut() -> Result<()>,
    {
        guard()?;
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
        for pair in self.ids.windows(2) {
            guard()?;
            if pair[0] >= pair[1] {
                return Err(Error::PointSchema(
                    "point identity column is not sorted and unique".to_owned(),
                ));
            }
        }
        for (index, column) in self.extensions.iter().enumerate() {
            guard()?;
            if column.id.0 < 0x8000_0000
                || column.stride == 0
                || index > 0 && self.extensions[index - 1].id >= column.id
                || column.bytes.len() != rows.saturating_mul(column.stride as usize)
            {
                return Err(Error::PointSchema(
                    "extension column violates registry/packing rules".to_owned(),
                ));
            }
        }
        guard()
    }

    /// Canonical big-endian packed bytes, used by cooks, manifests, and equality tests.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut sink = ByteSink::new();
        self.encode_canonical(&mut sink)?;
        Ok(sink.finish())
    }

    pub(crate) fn canonical_byte_len(&self) -> Result<usize> {
        self.validate()?;
        let mut sink = CountSink::new();
        self.encode_canonical(&mut sink)?;
        Ok(sink.finish())
    }

    pub(crate) fn encode_canonical<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        sink.write(b"SVEGPT01")?;
        sink.write(&point_schema_hash())?;
        sink.write(&(self.ids.len() as u64).to_be_bytes())?;
        for row in 0..self.ids.len() {
            sink.write(&self.ids[row].bytes())?;
            sink.write(&self.owner_cells[row].canonical_bytes())?;
            sink.write(&self.positions[row].cell().canonical_bytes())?;
            for tick in self.positions[row].local().ticks() {
                sink.write(&tick.to_be_bytes())?;
            }
            for lane in self.orientations[row].bits() {
                sink.write(&lane.to_be_bytes())?;
            }
            for scale in self.scales[row] {
                sink.write(&scale.canonical_bytes())?;
            }
            for tick in self.bounds[row].min_ticks() {
                sink.write(&tick.to_be_bytes())?;
            }
            for tick in self.bounds[row].max_ticks_exclusive() {
                sink.write(&tick.to_be_bytes())?;
            }
            sink.write(&self.families[row].value().to_be_bytes())?;
            sink.write(&self.variations[row].to_be_bytes())?;
            sink.write(&(self.lifecycles[row] as u32).to_be_bytes())?;
            sink.write(&self.phenotypes[row].to_be_bytes())?;
            sink.write(&self.representation_classes[row].to_be_bytes())?;
            sink.write(&self.deterministic_keys[row].to_be_bytes())?;
            sink.write(&self.candidates[row].to_be_bytes())?;
            push_optional_id(sink, self.parents[row])?;
            push_optional_id(sink, self.colonies[row])?;
            sink.write(&self.ecology_ticks[row].to_be_bytes())?;
            sink.write(&self.health[row].canonical_bytes())?;
            sink.write(&self.moisture[row].canonical_bytes())?;
            sink.write(&self.fuel[row].canonical_bytes())?;
            sink.write(&self.phenology[row].canonical_bytes())?;
            sink.write(&self.flags[row].bits().to_be_bytes())?;
            sink.write(&(self.interaction_policies[row] as u32).to_be_bytes())?;
            sink.write(&self.provenance[row].to_be_bytes())?;
            push_attachment(sink, self.attachments[row])?;
            for coordinate in self.surface_projections[row] {
                sink.write(&coordinate.canonical_bytes())?;
            }
        }
        sink.write(&(self.extensions.len() as u32).to_be_bytes())?;
        for column in &self.extensions {
            sink.write(&column.id.0.to_be_bytes())?;
            sink.write_byte(column.element_type as u8)?;
            sink.write(&column.stride.to_be_bytes())?;
            sink.write(&(column.bytes.len() as u64).to_be_bytes())?;
            sink.write(&column.bytes)?;
        }
        Ok(())
    }
}

fn read_optional_plant_id(reader: &mut BinaryReader<'_>) -> Result<Option<PlantId>> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(PlantId::from_bytes(reader.array()?)?)),
        _ => Err(Error::ArtifactFormat {
            format: "vegetation macro points",
            field: "optionalPlantId".to_owned(),
        }),
    }
}

fn read_attachment(reader: &mut BinaryReader<'_>) -> Result<Option<SurfaceAttachment>> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(SurfaceAttachment::new(
            SurfaceProviderId(reader.u64()?),
            SurfacePrimitiveId(reader.u64()?),
            [
                UnitInterval::from_bits(reader.u16()?),
                UnitInterval::from_bits(reader.u16()?),
                UnitInterval::from_bits(reader.u16()?),
            ],
            SurfaceRevision(reader.u64()?),
        )?)),
        _ => Err(Error::ArtifactFormat {
            format: "vegetation macro points",
            field: "surfaceAttachment".to_owned(),
        }),
    }
}

pub(crate) fn point_column_type_from_id(value: u8) -> Option<PointColumnType> {
    Some(match value {
        1 => PointColumnType::Id128,
        2 => PointColumnType::WorldCell,
        4 => PointColumnType::Orientation,
        5 => PointColumnType::FixedVec3,
        6 => PointColumnType::WorldBounds,
        7 => PointColumnType::AssetUuid,
        8 => PointColumnType::U32,
        9 => PointColumnType::U64,
        10 => PointColumnType::OptionalId128,
        11 => PointColumnType::Unit,
        12 => PointColumnType::SurfaceProjection,
        13 => PointColumnType::OptionalSurfaceAttachment,
        14 => PointColumnType::WorldPosition,
        _ => return None,
    })
}

fn push_optional_id<S: CanonicalSink>(sink: &mut S, id: Option<PlantId>) -> Result<()> {
    match id {
        Some(id) => {
            sink.write_byte(1)?;
            sink.write(&id.bytes())?;
        }
        None => sink.write_byte(0)?,
    }
    Ok(())
}

fn push_attachment<S: CanonicalSink>(
    sink: &mut S,
    attachment: Option<SurfaceAttachment>,
) -> Result<()> {
    match attachment {
        Some(attachment) => {
            sink.write_byte(1)?;
            sink.write(&attachment.provider.0.to_be_bytes())?;
            sink.write(&attachment.primitive.0.to_be_bytes())?;
            for weight in attachment.barycentric {
                sink.write(&weight.canonical_bytes())?;
            }
            sink.write(&attachment.revision.0.to_be_bytes())?;
        }
        None => sink.write_byte(0)?,
    }
    Ok(())
}

/// A representative sprout at the centre-plus-one tick of `cell`, for tests that need a valid row
/// rather than a particular one.
#[cfg(test)]
pub(crate) fn sample_plant_point(id: PlantId, cell: WorldCellKey) -> PlantPoint {
    let position = WorldPosition::from_global_ticks(
        cell.coordinates()
            .map(|coordinate| i128::from(coordinate) * 262_144 + 1),
    )
    .unwrap();
    PlantPoint {
        id,
        owner: cell,
        position,
        orientation: QuantizedOrientation::identity(),
        scale: [DecisionScalar::from_integer(1).unwrap(); 3],
        bounds: saffron_spatial::WorldBounds::new(
            position.global_ticks().map(|tick| tick - 1),
            position.global_ticks().map(|tick| tick + 2),
        )
        .unwrap(),
        family: Uuid(7),
        variation: 0,
        lifecycle: PlantLifecycle::Sprout,
        phenotype: 0,
        representation_class: 0,
        deterministic_key: 8,
        candidate: 9,
        parent: None,
        colony: None,
        ecology_tick: 10,
        health: UnitInterval::ONE,
        moisture: UnitInterval::from_bits(20_000),
        fuel: UnitInterval::from_bits(30_000),
        phenology: UnitInterval::ZERO,
        flags: PlantFlags::RUNTIME,
        interaction_policy: InteractionPolicy::Interactive,
        provenance: 0,
        attachment: None,
        surface_projection: [DecisionScalar::from_bits(0); 3],
    }
}

#[cfg(test)]
mod tests {
    use crate::identity::derive_procedural_plant_id;
    use saffron_spatial::{DecisionScalar, WorldBounds};

    use super::*;
    use crate::ProceduralPlantIdentity;

    fn point(candidate: u64) -> PlantPoint {
        let position = WorldPosition::from_global_ticks([candidate as i128, 0, 0]).unwrap();
        PlantPoint {
            id: derive_procedural_plant_id(ProceduralPlantIdentity {
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
        let a = PlantPointColumns::from_points(vec![point(1), point(2)]).unwrap();
        let b = PlantPointColumns::from_points(vec![point(1), point(2)]).unwrap();
        assert_eq!(a.canonical_bytes().unwrap(), b.canonical_bytes().unwrap());
        assert_eq!(&a.canonical_bytes().unwrap()[8..40], &point_schema_hash());
    }

    #[test]
    fn extensions_are_registered_sorted_and_row_sized() {
        let mut columns = PlantPointColumns::from_points(vec![point(1), point(2)]).unwrap();
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
    fn canonical_decoder_projects_fixed_and_extension_rows() {
        let mut columns = PlantPointColumns::from_points(vec![point(1), point(2)]).unwrap();
        columns
            .add_extension(ExtensionColumn {
                id: PointColumnId(0x8000_0001),
                element_type: PointColumnType::U32,
                stride: 4,
                bytes: vec![0, 1, 2, 3, 4, 5, 6, 7],
            })
            .unwrap();
        let bytes = columns.canonical_bytes().unwrap();
        let decoded = PlantPointColumns::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded, columns);
        assert_eq!(decoded.row(1).unwrap().point, columns.point(1).unwrap());
        assert_eq!(decoded.row(1).unwrap().extensions[0].bytes, &[4, 5, 6, 7]);

        let mut wrong_schema = bytes.clone();
        wrong_schema[8] ^= 0xff;
        assert!(matches!(
            PlantPointColumns::from_canonical_bytes(&wrong_schema),
            Err(Error::ArtifactFormat { field, .. }) if field == "schemaHash"
        ));
    }

    #[test]
    fn duplicate_identity_is_rejected_before_publication() {
        let duplicate = point(1);
        assert!(PlantPointColumns::from_points(vec![duplicate.clone(), duplicate]).is_err());
    }
}
