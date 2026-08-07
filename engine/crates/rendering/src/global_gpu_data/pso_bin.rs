//! The PSO bin key and material class: the packed permutation words the executor sorts draws
//! by and the shaders decode.

use super::*;

/// Fixed semantic PSO-bin vocabulary independent of the draw executor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Pod, Zeroable)]
#[repr(transparent)]
pub struct GpuPsoBin(pub(super) u32);

/// Pass-independent immutable material pipeline dimensions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Pod, Zeroable)]
#[repr(transparent)]
pub struct GpuMaterialClass(pub(super) u32);

/// Geometry representation consumed by a draw executor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuRepresentation {
    /// Portable triangle-cluster content, independent of indexed or mesh-shader execution.
    #[default]
    TriangleCluster = 0,
    /// Aggregate voxel surface.
    AggregateVoxel = 1,
    /// Procedural micro vegetation blade reconstructed from a resident field tile.
    MicroBlade = 2,
    /// Amplified displaced micro-geometry read from the frame's displacement arena.
    DisplacedMicro = 3,
}

/// Raster sidedness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuSidedness {
    /// Back-face culling is enabled.
    #[default]
    Single = 0,
    /// Both sides are rasterized.
    Double = 1,
}

/// Alpha compositing class kept distinct from coverage classification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuTransparency {
    /// Depth-writing opaque or cutout geometry.
    #[default]
    Opaque = 0,
    /// Back-to-front alpha blending.
    AlphaBlended = 1,
}

/// Shared deformation-provider output class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuDeformation {
    /// Rigid geometry.
    #[default]
    Rigid = 0,
    /// Common deformed output produced by one or more composed deformation providers.
    Deformed = 1,
}

/// Render-pass class sharing one semantic visibility record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuPassClass {
    /// Depth prepass.
    #[default]
    Depth = 0,
    /// Forward main pass.
    Main = 1,
    /// Motion-vector pass.
    Motion = 2,
    /// Directional, spot, point, or virtual shadow depth.
    Shadow = 3,
    /// Deferred G-buffer pass.
    GBuffer = 4,
    /// Sorted alpha-blended pass.
    Transparent = 5,
    /// Selection-ID pass.
    Selection = 6,
    /// Asset thumbnail or preview pass.
    Preview = 7,
    /// Ray-tracing geometry/hit classification.
    RayTracing = 8,
    /// Wireframe and scene-debug geometry.
    WireDebug = 9,
}

impl GpuMaterialClass {
    /// Builds one immutable class from the canonical material dimensions.
    pub const fn new(
        coverage: AlphaClassification,
        sidedness: GpuSidedness,
        surface_model: SurfaceModel,
        transparency: GpuTransparency,
        unlit: bool,
    ) -> Self {
        let surface_model = match surface_model {
            SurfaceModel::Standard => 0,
            SurfaceModel::ThinSheetFoliage => 1,
        };
        Self(
            ((coverage as u32) << GPU_MATERIAL_COVERAGE_SHIFT)
                | ((sidedness as u32) << GPU_MATERIAL_SIDEDNESS_SHIFT)
                | (surface_model << GPU_MATERIAL_SURFACE_MODEL_SHIFT)
                | ((transparency as u32) << GPU_MATERIAL_TRANSPARENCY_SHIFT)
                | ((unlit as u32) << GPU_MATERIAL_UNLIT_SHIFT),
        )
    }

    /// Strictly restores a bounded material class from GPU bits.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !0x3f != 0 || bits & 0x3 > 2 {
            return None;
        }
        Some(Self(bits))
    }

    /// The unlit shading permutation.
    #[must_use]
    pub const fn unlit(self) -> bool {
        self.0 & (1 << GPU_MATERIAL_UNLIT_SHIFT) != 0
    }

    /// Canonical scalar embedded in material and full PSO records.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Canonical coverage classification.
    #[must_use]
    pub const fn coverage(self) -> AlphaClassification {
        match self.0 & 0x3 {
            0 => AlphaClassification::Opaque,
            1 => AlphaClassification::Masked,
            2 => AlphaClassification::Transmissive,
            _ => panic!("GpuMaterialClass must contain validated coverage bits"),
        }
    }

    /// Raster sidedness.
    #[must_use]
    pub const fn sidedness(self) -> GpuSidedness {
        if (self.0 >> 2) & 1 == 0 {
            GpuSidedness::Single
        } else {
            GpuSidedness::Double
        }
    }

    /// Canonical material surface model.
    #[must_use]
    pub const fn surface_model(self) -> SurfaceModel {
        if (self.0 >> 3) & 1 == 0 {
            SurfaceModel::Standard
        } else {
            SurfaceModel::ThinSheetFoliage
        }
    }

    /// Transparency compositing class.
    #[must_use]
    pub const fn transparency(self) -> GpuTransparency {
        if (self.0 >> 4) & 1 == 0 {
            GpuTransparency::Opaque
        } else {
            GpuTransparency::AlphaBlended
        }
    }
}

impl GpuPsoBin {
    /// Builds a bin from the canonical bounded dimensions.
    pub const fn new(
        representation: GpuRepresentation,
        material: GpuMaterialClass,
        deformation: GpuDeformation,
        pass: GpuPassClass,
    ) -> Self {
        Self(
            ((representation as u32) << GPU_PSO_REPRESENTATION_SHIFT)
                | (material.bits() << GPU_PSO_MATERIAL_SHIFT)
                | ((deformation as u32) << GPU_PSO_DEFORMATION_SHIFT)
                | ((pass as u32) << GPU_PSO_PASS_SHIFT),
        )
    }

    /// Validates and restores the complete bounded PSO vocabulary from GPU bits.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !0x1fff != 0 || (bits >> 2) & 0x3 > 2 || (bits >> GPU_PSO_PASS_SHIFT) & 0xf > 9 {
            return None;
        }
        Some(Self(bits))
    }

    /// Canonical scalar stored in table and draw records.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Representation dimension.
    #[must_use]
    pub const fn representation(self) -> GpuRepresentation {
        match self.0 & 0x3 {
            0 => GpuRepresentation::TriangleCluster,
            1 => GpuRepresentation::AggregateVoxel,
            2 => GpuRepresentation::MicroBlade,
            _ => GpuRepresentation::DisplacedMicro,
        }
    }

    /// Coverage dimension.
    #[must_use]
    pub const fn coverage(self) -> AlphaClassification {
        self.material_class().coverage()
    }

    /// Sidedness dimension.
    #[must_use]
    pub const fn sidedness(self) -> GpuSidedness {
        self.material_class().sidedness()
    }

    /// Surface-model dimension.
    #[must_use]
    pub const fn surface_model(self) -> SurfaceModel {
        self.material_class().surface_model()
    }

    /// Transparency dimension.
    #[must_use]
    pub const fn transparency(self) -> GpuTransparency {
        self.material_class().transparency()
    }

    /// Pass-independent dimensions sourced from the immutable material record.
    #[must_use]
    pub const fn material_class(self) -> GpuMaterialClass {
        GpuMaterialClass((self.0 >> GPU_PSO_MATERIAL_SHIFT) & 0x1f)
    }

    /// Deformation dimension.
    #[must_use]
    pub const fn deformation(self) -> GpuDeformation {
        match (self.0 >> GPU_PSO_DEFORMATION_SHIFT) & 0x1 {
            0 => GpuDeformation::Rigid,
            1 => GpuDeformation::Deformed,
            _ => panic!("GpuPsoBin must contain validated deformation bits"),
        }
    }

    /// Render-pass dimension.
    #[must_use]
    pub const fn pass(self) -> GpuPassClass {
        match (self.0 >> GPU_PSO_PASS_SHIFT) & 0xf {
            0 => GpuPassClass::Depth,
            1 => GpuPassClass::Main,
            2 => GpuPassClass::Motion,
            3 => GpuPassClass::Shadow,
            4 => GpuPassClass::GBuffer,
            5 => GpuPassClass::Transparent,
            6 => GpuPassClass::Selection,
            7 => GpuPassClass::Preview,
            8 => GpuPassClass::RayTracing,
            9 => GpuPassClass::WireDebug,
            _ => panic!("GpuPsoBin must contain validated pass bits"),
        }
    }
}
