//! The project bring-up lifecycle state: the phase, the ordered boot stages, the per-frame
//! progress snapshot, and the load-request inbox the host loader reads.

/// The project bring-up lifecycle. The single authoritative phase the dispatch gate and the
/// `project-status` command read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectPhase {
    /// No project is open.
    #[default]
    Unloaded,
    /// A load is in flight (doc parse, install, residency, warmup). Set by the loader.
    Loading,
    /// A project is fully open and resident.
    Ready,
    /// The last load failed; the progress snapshot carries the message.
    Failed,
}

/// The ordered boot stages within [`ProjectPhase::Loading`]. Reported through
/// [`ProjectLoadProgress`]; Phase 3 mirrors it to a wire `BootStageDto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BootStage {
    /// Reading + parsing `project.json` (off-thread, indeterminate).
    #[default]
    Manifest,
    /// Reconciling the asset catalog against disk (off-thread; determinate `done`/`total`).
    Catalog,
    /// Deserializing the scene graph (main thread, indeterminate).
    Scene,
    /// Installing the loaded doc: idle + swap scene/catalog (main thread, one frame).
    Install,
    /// GPU residency: mesh + texture uploads (main thread, stepped; determinate `done`/`total`).
    Assets,
    /// Resolving the sky panorama + environment (main thread, warmup).
    Skybox,
    /// Priming acceleration structures / first frame (main thread, warmup).
    Accel,
    /// Terminal success.
    Ready,
    /// Terminal failure (the progress `error` carries the message).
    Failed,
}

/// The per-frame progress snapshot the loader writes and the `project-status` command reads.
/// `total == 0` means indeterminate (a spinner, not a filled bar). `version` is monotonic; the
/// editor dedups on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectLoadProgress {
    /// The current boot stage.
    pub stage: BootStage,
    /// Determinate progress numerator (`0` with `total == 0` ⇒ indeterminate).
    pub done: u32,
    /// Determinate progress denominator (`0` ⇒ indeterminate).
    pub total: u32,
    /// The engine-supplied human line ("Loading assets 12/40", "Scanning assets", …).
    pub label: String,
    /// The item currently being worked (a file / asset name), or empty.
    pub current_item: String,
    /// The failure message; empty unless `stage == Failed`.
    pub error: String,
    /// Monotonic; bumped on every loader transition so the editor dedups its poll.
    pub version: u64,
}

/// The `new-project` spec handed to the loader — mirrors the `saffron-assets` `NewProject`
/// without a dependency inversion (the host maps between them).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewProjectSpec {
    /// The short project name (must pass the assets-side `valid_project_name`).
    pub name: String,
    /// The human display name, or empty to derive from `name`.
    pub display_name: String,
    /// The project root directory, or empty to place it under the userdata root.
    pub root: String,
}

/// One request handed from a command handler / bootstrap to the host loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectLoadRequest {
    /// Open the project at the given selection path/name.
    Open(String),
    /// Create a fresh project from the spec.
    New(NewProjectSpec),
    /// Re-open the currently-open project from its own path.
    Reload,
}
