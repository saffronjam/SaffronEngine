//! Vulkan adapter for vegetation graph execution and conformance evidence.

#![deny(unsafe_code)]

mod conformance;
mod executor;

pub use conformance::*;
pub use executor::VulkanGraphComputeExecutor;

/// Vegetation GPU adapter failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A generic Vulkan compute or artifact operation failed.
    #[error(transparent)]
    Rendering(#[from] saffron_rendering::Error),
    /// The selected device is not a physical integrated or discrete GPU.
    #[error("vegetation graph qualification requires a physical GPU, found {device_type}")]
    NonPhysicalDevice {
        /// Vulkan physical-device class.
        device_type: String,
    },
    /// The bounded qualification deadline cannot be represented.
    #[error("vegetation graph qualification deadline overflowed")]
    QualificationDeadlineOverflow,
    /// Vulkan validation reported issues during conformance capture.
    #[error("vegetation compute conformance raised {count} Vulkan validation issues")]
    ValidationIssues {
        /// New warning/error count.
        count: u64,
    },
    /// Graph qualification selected a profile different from the Vulkan capture.
    #[error("vegetation graph qualification profile differs from the Vulkan profile")]
    ProfileMismatch,
    /// Graph qualification produced no operator evidence.
    #[error("vegetation graph qualification produced no operator evidence")]
    MissingQualificationEvidence,
    /// Graph qualification evidence differs from the canonical corpus.
    #[error("vegetation graph qualification evidence is not canonical")]
    NoncanonicalQualificationEvidence,
    /// Vegetation graph qualification failed.
    #[error(transparent)]
    Vegetation(#[from] saffron_vegetation::Error),
}

/// Vegetation GPU adapter result.
pub type Result<T> = std::result::Result<T, Error>;
