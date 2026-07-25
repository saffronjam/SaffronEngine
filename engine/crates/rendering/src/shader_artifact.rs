//! Strict runtime verification for generated shader artifacts and compile inputs.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const MANIFEST_NAME: &str = "shader-artifacts.generated.json";
const MANIFEST_SCHEMA_VERSION: u32 = 1;
const MANIFEST_MAX_BYTES: u64 = 8 * 1024 * 1024;
const SOURCE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const ARTIFACT_MAX_BYTES: u64 = 256 * 1024 * 1024;
const COMPILE_INPUT_HASH_DOMAIN: &[u8] = b"saffron-anima/shader-compile-input/v1\0";
const COMPILER_IDENTITY_HASH_DOMAIN: &[u8] = b"saffron-anima/shader-compiler-identity/v1\0";
const ARTIFACT_IDENTITY_HASH_DOMAIN: &[u8] = b"saffron-anima/shader-artifact-identity/v1\0";
const SPIRV_CAPABILITIES: &str = "SPV_KHR_non_semantic_info+SPV_GOOGLE_user_type+spvSparseResidency+spvMinLod+spvFragmentFullyCoveredEXT+spvShaderNonUniformEXT+spvRayQueryKHR+spvMeshShadingEXT+spvGroupNonUniform+spvGroupNonUniformBallot";
const EXPECTED_SPIRV_FLAGS: &[&str] = &[
    "-profile",
    "glsl_450",
    "-target",
    "spirv",
    "-emit-spirv-directly",
    "-fvk-use-entrypoint-name",
    "-matrix-layout-column-major",
    "-capability",
    SPIRV_CAPABILITIES,
];
/// One exact SHA-256 identity used by the generated shader pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShaderSha256([u8; 32]);

impl ShaderSha256 {
    /// Raw digest bytes.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    fn digest(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    fn parse(field: &'static str, shader: &str, value: &str) -> Result<Self, ShaderArtifactError> {
        if value.len() != 64 || value.bytes().any(|byte| !byte.is_ascii_hexdigit()) {
            return Err(ShaderArtifactError::InvalidHash {
                shader: shader.to_owned(),
                field,
            });
        }
        if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(ShaderArtifactError::InvalidHash {
                shader: shader.to_owned(),
                field,
            });
        }
        let mut digest = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_nibble(pair[0]).ok_or_else(|| ShaderArtifactError::InvalidHash {
                shader: shader.to_owned(),
                field,
            })?;
            let low = hex_nibble(pair[1]).ok_or_else(|| ShaderArtifactError::InvalidHash {
                shader: shader.to_owned(),
                field,
            })?;
            digest[index] = high << 4 | low;
        }
        Ok(Self(digest))
    }
}

impl fmt::Display for ShaderSha256 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Immutable identity of one verified SPIR-V artifact and every input that compiled it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderArtifactIdentity {
    shader: String,
    source: String,
    artifact: String,
    source_files: Vec<String>,
    compile_input_sha256: ShaderSha256,
    spirv_sha256: ShaderSha256,
    compiler_identity: String,
    compiler_identity_sha256: ShaderSha256,
    spirv_flags: Vec<String>,
    defines: Vec<String>,
    record_sha256: ShaderSha256,
}

/// Exact generated shader name, entry source, closure, and definition contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShaderArtifactContract {
    shader: &'static str,
    source: &'static str,
    artifact: &'static str,
    source_files: &'static [&'static str],
    defines: &'static [&'static str],
}

impl ShaderArtifactContract {
    /// Creates one compile-time artifact contract.
    #[must_use]
    pub const fn new(
        shader: &'static str,
        source: &'static str,
        artifact: &'static str,
        source_files: &'static [&'static str],
        defines: &'static [&'static str],
    ) -> Self {
        Self {
            shader,
            source,
            artifact,
            source_files,
            defines,
        }
    }

    /// Logical shader variant selected from the generated manifest.
    #[must_use]
    pub const fn shader(self) -> &'static str {
        self.shader
    }

    /// Verifies one loaded identity against every exact contract field.
    pub fn verify(self, identity: &ShaderArtifactIdentity) -> Result<(), ShaderArtifactError> {
        let source_files_match = identity
            .source_files
            .iter()
            .map(String::as_str)
            .eq(self.source_files.iter().copied());
        let defines_match = identity
            .defines
            .iter()
            .map(String::as_str)
            .eq(self.defines.iter().copied());
        if identity.shader != self.shader
            || identity.source != self.source
            || identity.artifact != self.artifact
            || !source_files_match
            || !defines_match
        {
            return Err(ShaderArtifactError::ContractMismatch {
                shader: self.shader.to_owned(),
            });
        }
        Ok(())
    }
}

impl ShaderArtifactIdentity {
    /// Logical shader variant name.
    #[must_use]
    pub fn shader(&self) -> &str {
        &self.shader
    }

    /// Entry-point source named by the compiler pipeline.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Runtime SPIR-V artifact filename.
    #[must_use]
    pub fn artifact(&self) -> &str {
        &self.artifact
    }

    /// Canonical compiler-resolved transitive source closure.
    #[must_use]
    pub fn source_files(&self) -> &[String] {
        &self.source_files
    }

    /// Exact compile-input identity over flags, defines, source names, and source bytes.
    #[must_use]
    pub const fn compile_input_sha256(&self) -> ShaderSha256 {
        self.compile_input_sha256
    }

    /// Exact loaded SPIR-V bytes identity.
    #[must_use]
    pub const fn spirv_sha256(&self) -> ShaderSha256 {
        self.spirv_sha256
    }

    /// Exact `slangc -version` identity captured by xtask.
    #[must_use]
    pub fn compiler_identity(&self) -> &str {
        &self.compiler_identity
    }

    /// Stable hash of the compiler identity.
    #[must_use]
    pub const fn compiler_identity_sha256(&self) -> ShaderSha256 {
        self.compiler_identity_sha256
    }

    /// Exact ordered SPIR-V compiler flags.
    #[must_use]
    pub fn spirv_flags(&self) -> &[String] {
        &self.spirv_flags
    }

    /// Exact ordered preprocessor definitions.
    #[must_use]
    pub fn defines(&self) -> &[String] {
        &self.defines
    }

    /// Canonical identity of the complete verified record.
    #[must_use]
    pub const fn record_sha256(&self) -> ShaderSha256 {
        self.record_sha256
    }
}

/// Strict shader-artifact manifest or runtime-file validation failure.
#[derive(Debug, thiserror::Error)]
pub enum ShaderArtifactError {
    /// A manifest, source, or artifact could not be read.
    #[error("cannot read shader {kind} '{}': {source}", path.display())]
    FileRead {
        /// Runtime file class.
        kind: &'static str,
        /// Exact failed path.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// A runtime file exceeds the strict reader's allocation ceiling.
    #[error("shader {kind} '{}' is {bytes} bytes, limit {limit}", path.display())]
    FileTooLarge {
        /// Runtime file class.
        kind: &'static str,
        /// Exact failed path.
        path: PathBuf,
        /// Observed byte count.
        bytes: u64,
        /// Maximum admitted byte count.
        limit: u64,
    },
    /// The manifest is not the exact typed JSON schema.
    #[error("malformed shader artifact manifest '{}': {source}", path.display())]
    MalformedManifest {
        /// Manifest path.
        path: PathBuf,
        /// Strict JSON decode failure.
        #[source]
        source: serde_json::Error,
    },
    /// The manifest schema version is not the runtime version.
    #[error("shader artifact manifest schema {found} is unsupported; expected {expected}")]
    UnsupportedSchema {
        /// Manifest schema.
        found: u32,
        /// Runtime schema.
        expected: u32,
    },
    /// The recorded compiler identity is empty or noncanonical.
    #[error("shader artifact manifest compiler identity is not canonical")]
    InvalidCompilerIdentity,
    /// The flags differ from the one supported runtime compile contract.
    #[error("shader artifact manifest SPIR-V flags do not match the runtime contract")]
    UnexpectedSpirvFlags,
    /// Artifact records are missing, duplicated, or not strictly name-sorted.
    #[error("shader artifact manifest records are not canonical")]
    NoncanonicalManifestRecords,
    /// One artifact record violates the generated naming or closure contract.
    #[error("shader artifact record '{shader}' is not canonical: {reason}")]
    NoncanonicalRecord {
        /// Logical shader name.
        shader: String,
        /// Closed validation reason.
        reason: &'static str,
    },
    /// One manifest digest is not canonical lowercase SHA-256.
    #[error("shader artifact record '{shader}' has invalid {field}")]
    InvalidHash {
        /// Logical shader name.
        shader: String,
        /// Digest field.
        field: &'static str,
    },
    /// The selected shader has no generated record.
    #[error("shader artifact manifest does not contain '{shader}'")]
    MissingShader {
        /// Requested logical shader name.
        shader: String,
    },
    /// A loaded record differs from the complete contract supplied by its consumer.
    #[error("shader artifact record '{shader}' does not match its consumer contract")]
    ContractMismatch {
        /// Logical shader name required by the consumer.
        shader: String,
    },
    /// Runtime source bytes no longer match the compile-input identity.
    #[error("shader '{shader}' compile-input hash mismatch: manifest {expected}, runtime {actual}")]
    CompileInputMismatch {
        /// Logical shader name.
        shader: String,
        /// Manifest identity.
        expected: ShaderSha256,
        /// Recomputed runtime identity.
        actual: ShaderSha256,
    },
    /// Runtime SPIR-V bytes no longer match the generated artifact identity.
    #[error("shader '{shader}' SPIR-V hash mismatch: manifest {expected}, runtime {actual}")]
    SpirvMismatch {
        /// Logical shader name.
        shader: String,
        /// Manifest identity.
        expected: ShaderSha256,
        /// Recomputed runtime identity.
        actual: ShaderSha256,
    },
    /// A length cannot be represented in the canonical hash encoding.
    #[error("shader artifact identity input exceeds the canonical u64 length")]
    LengthOverflow,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    slangc_version: String,
    spirv_flags: Vec<String>,
    artifacts: Vec<ManifestEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestEntry {
    shader: String,
    source: String,
    artifact: String,
    defines: Vec<String>,
    source_files: Vec<String>,
    compile_input_sha256: String,
    spirv_sha256: String,
}

pub(crate) fn load_shader_artifact(
    shader_dir: &Path,
    shader: &str,
) -> Result<(ShaderArtifactIdentity, Vec<u8>), ShaderArtifactError> {
    if !canonical_shader_name(shader) {
        return Err(ShaderArtifactError::NoncanonicalRecord {
            shader: shader.to_owned(),
            reason: "requested shader name is not canonical",
        });
    }
    let manifest_path = shader_dir.join(MANIFEST_NAME);
    let manifest_bytes = read_limited(&manifest_path, "manifest", MANIFEST_MAX_BYTES)?;
    let manifest = serde_json::from_slice::<Manifest>(&manifest_bytes).map_err(|source| {
        ShaderArtifactError::MalformedManifest {
            path: manifest_path,
            source,
        }
    })?;
    validate_manifest(&manifest)?;
    let index = manifest
        .artifacts
        .binary_search_by(|entry| entry.shader.as_str().cmp(shader))
        .map_err(|_| ShaderArtifactError::MissingShader {
            shader: shader.to_owned(),
        })?;
    let entry = &manifest.artifacts[index];
    let compile_input_expected =
        ShaderSha256::parse("compileInputSha256", shader, &entry.compile_input_sha256)?;
    let spirv_expected = ShaderSha256::parse("spirvSha256", shader, &entry.spirv_sha256)?;
    let compile_input_actual = compile_input_sha256(
        &shader_dir.join("source"),
        &entry.source_files,
        &manifest.spirv_flags,
        &entry.defines,
    )?;
    if compile_input_actual != compile_input_expected {
        return Err(ShaderArtifactError::CompileInputMismatch {
            shader: shader.to_owned(),
            expected: compile_input_expected,
            actual: compile_input_actual,
        });
    }
    let artifact_path = shader_dir.join(&entry.artifact);
    let spirv_bytes = read_limited(&artifact_path, "SPIR-V artifact", ARTIFACT_MAX_BYTES)?;
    let spirv_actual = ShaderSha256::digest(&spirv_bytes);
    if spirv_actual != spirv_expected {
        return Err(ShaderArtifactError::SpirvMismatch {
            shader: shader.to_owned(),
            expected: spirv_expected,
            actual: spirv_actual,
        });
    }
    let compiler_identity_sha256 = compiler_identity_sha256(&manifest.slangc_version)?;
    let record_sha256 = record_sha256(
        entry,
        &manifest.slangc_version,
        compiler_identity_sha256,
        &manifest.spirv_flags,
        compile_input_actual,
        spirv_actual,
    )?;
    Ok((
        ShaderArtifactIdentity {
            shader: entry.shader.clone(),
            source: entry.source.clone(),
            artifact: entry.artifact.clone(),
            source_files: entry.source_files.clone(),
            compile_input_sha256: compile_input_actual,
            spirv_sha256: spirv_actual,
            compiler_identity: manifest.slangc_version,
            compiler_identity_sha256,
            spirv_flags: manifest.spirv_flags,
            defines: entry.defines.clone(),
            record_sha256,
        },
        spirv_bytes,
    ))
}

fn validate_manifest(manifest: &Manifest) -> Result<(), ShaderArtifactError> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(ShaderArtifactError::UnsupportedSchema {
            found: manifest.schema_version,
            expected: MANIFEST_SCHEMA_VERSION,
        });
    }
    if manifest.slangc_version.is_empty()
        || manifest.slangc_version.trim() != manifest.slangc_version
        || manifest.slangc_version.contains('\0')
    {
        return Err(ShaderArtifactError::InvalidCompilerIdentity);
    }
    if manifest.spirv_flags.len() != EXPECTED_SPIRV_FLAGS.len()
        || manifest
            .spirv_flags
            .iter()
            .map(String::as_str)
            .ne(EXPECTED_SPIRV_FLAGS.iter().copied())
    {
        return Err(ShaderArtifactError::UnexpectedSpirvFlags);
    }
    if manifest.artifacts.is_empty()
        || !manifest
            .artifacts
            .windows(2)
            .all(|pair| pair[0].shader < pair[1].shader)
    {
        return Err(ShaderArtifactError::NoncanonicalManifestRecords);
    }
    for entry in &manifest.artifacts {
        validate_record(entry)?;
    }
    Ok(())
}

fn validate_record(entry: &ManifestEntry) -> Result<(), ShaderArtifactError> {
    if !canonical_shader_name(&entry.shader) {
        return noncanonical(entry, "shader name is not lowercase snake case");
    }
    if entry.artifact != format!("{}.spv", entry.shader) {
        return noncanonical(entry, "artifact filename does not match the shader name");
    }
    if !canonical_source_name(&entry.source) {
        return noncanonical(entry, "entry source path is not canonical");
    }
    if entry.source_files.is_empty()
        || !entry.source_files.windows(2).all(|pair| pair[0] < pair[1])
        || !entry
            .source_files
            .iter()
            .all(|path| canonical_source_name(path))
        || entry.source_files.binary_search(&entry.source).is_err()
    {
        return noncanonical(entry, "source closure is not sorted, unique, and complete");
    }
    if entry.defines.iter().any(|define| {
        define.is_empty() || define.trim() != define || define.chars().any(char::is_control)
    }) || entry
        .defines
        .iter()
        .enumerate()
        .any(|(index, define)| entry.defines[..index].contains(define))
    {
        return noncanonical(entry, "preprocessor definitions are not canonical");
    }
    ShaderSha256::parse(
        "compileInputSha256",
        &entry.shader,
        &entry.compile_input_sha256,
    )?;
    ShaderSha256::parse("spirvSha256", &entry.shader, &entry.spirv_sha256)?;
    Ok(())
}

fn noncanonical<T>(entry: &ManifestEntry, reason: &'static str) -> Result<T, ShaderArtifactError> {
    Err(ShaderArtifactError::NoncanonicalRecord {
        shader: entry.shader.clone(),
        reason,
    })
}

fn canonical_shader_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn canonical_source_name(value: &str) -> bool {
    let path = Path::new(value);
    path.extension().and_then(|extension| extension.to_str()) == Some("slang")
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && path.to_str() == Some(value)
}

fn read_limited(
    path: &Path,
    kind: &'static str,
    limit: u64,
) -> Result<Vec<u8>, ShaderArtifactError> {
    let metadata = std::fs::metadata(path).map_err(|source| ShaderArtifactError::FileRead {
        kind,
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > limit {
        return Err(ShaderArtifactError::FileTooLarge {
            kind,
            path: path.to_path_buf(),
            bytes: metadata.len(),
            limit,
        });
    }
    let bytes = std::fs::read(path).map_err(|source| ShaderArtifactError::FileRead {
        kind,
        path: path.to_path_buf(),
        source,
    })?;
    let bytes_len = u64::try_from(bytes.len()).map_err(|_| ShaderArtifactError::LengthOverflow)?;
    if bytes_len > limit {
        return Err(ShaderArtifactError::FileTooLarge {
            kind,
            path: path.to_path_buf(),
            bytes: bytes_len,
            limit,
        });
    }
    Ok(bytes)
}

fn compile_input_sha256(
    source_dir: &Path,
    source_files: &[String],
    spirv_flags: &[String],
    defines: &[String],
) -> Result<ShaderSha256, ShaderArtifactError> {
    let mut hasher = Sha256::new();
    hasher.update(COMPILE_INPUT_HASH_DOMAIN);
    hash_string_sequence(
        &mut hasher,
        b"spirv-flags",
        spirv_flags.iter().map(String::as_str),
    )?;
    hash_string_sequence(&mut hasher, b"defines", defines.iter().map(String::as_str))?;
    hash_len(&mut hasher, source_files.len())?;
    for source in source_files {
        let bytes = read_limited(&source_dir.join(source), "compile source", SOURCE_MAX_BYTES)?;
        hash_bytes(&mut hasher, source.as_bytes())?;
        hash_bytes(&mut hasher, &bytes)?;
    }
    Ok(ShaderSha256(hasher.finalize().into()))
}

fn compiler_identity_sha256(value: &str) -> Result<ShaderSha256, ShaderArtifactError> {
    let mut hasher = Sha256::new();
    hasher.update(COMPILER_IDENTITY_HASH_DOMAIN);
    hash_bytes(&mut hasher, value.as_bytes())?;
    Ok(ShaderSha256(hasher.finalize().into()))
}

fn record_sha256(
    entry: &ManifestEntry,
    compiler_identity: &str,
    compiler_identity_sha256: ShaderSha256,
    spirv_flags: &[String],
    compile_input_sha256: ShaderSha256,
    spirv_sha256: ShaderSha256,
) -> Result<ShaderSha256, ShaderArtifactError> {
    let mut hasher = Sha256::new();
    hasher.update(ARTIFACT_IDENTITY_HASH_DOMAIN);
    hash_bytes(&mut hasher, entry.shader.as_bytes())?;
    hash_bytes(&mut hasher, entry.source.as_bytes())?;
    hash_bytes(&mut hasher, entry.artifact.as_bytes())?;
    hash_string_sequence(
        &mut hasher,
        b"source-files",
        entry.source_files.iter().map(String::as_str),
    )?;
    hash_string_sequence(
        &mut hasher,
        b"spirv-flags",
        spirv_flags.iter().map(String::as_str),
    )?;
    hash_string_sequence(
        &mut hasher,
        b"defines",
        entry.defines.iter().map(String::as_str),
    )?;
    hash_bytes(&mut hasher, compiler_identity.as_bytes())?;
    hash_bytes(&mut hasher, &compiler_identity_sha256.0)?;
    hash_bytes(&mut hasher, &compile_input_sha256.0)?;
    hash_bytes(&mut hasher, &spirv_sha256.0)?;
    Ok(ShaderSha256(hasher.finalize().into()))
}

fn hash_string_sequence<'a>(
    hasher: &mut Sha256,
    label: &[u8],
    values: impl ExactSizeIterator<Item = &'a str>,
) -> Result<(), ShaderArtifactError> {
    hash_bytes(hasher, label)?;
    hash_len(hasher, values.len())?;
    for value in values {
        hash_bytes(hasher, value.as_bytes())?;
    }
    Ok(())
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), ShaderArtifactError> {
    hash_len(hasher, bytes.len())?;
    hasher.update(bytes);
    Ok(())
}

fn hash_len(hasher: &mut Sha256, len: usize) -> Result<(), ShaderArtifactError> {
    let len = u64::try_from(len).map_err(|_| ShaderArtifactError::LengthOverflow)?;
    hasher.update(len.to_be_bytes());
    Ok(())
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Value, json};

    use super::*;

    const FIXTURE_SHADER: &str = "compute_fixture";
    const FIXTURE_SOURCE: &str = "compute_fixture.slang";
    const FIXTURE_ARTIFACT: &str = "compute_fixture.spv";
    const FIXTURE_SOURCE_FILES: &[&str] = &["compute_fixture.slang", "spatial_numeric.slang"];

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        shader_dir: PathBuf,
        manifest: Value,
        spirv: Vec<u8>,
    }

    impl Fixture {
        fn new() -> Self {
            let identity = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let shader_dir = std::env::temp_dir().join(format!(
                "saffron_shader_artifact_{}_{}",
                std::process::id(),
                identity
            ));
            let source_dir = shader_dir.join("source");
            std::fs::create_dir_all(&source_dir).unwrap();
            std::fs::write(
                source_dir.join("spatial_numeric.slang"),
                b"const uint VALUE = 7;\n",
            )
            .unwrap();
            std::fs::write(
                source_dir.join(FIXTURE_SOURCE),
                b"import spatial_numeric;\n",
            )
            .unwrap();
            let flags = EXPECTED_SPIRV_FLAGS
                .iter()
                .map(|flag| (*flag).to_owned())
                .collect::<Vec<_>>();
            let source_files = FIXTURE_SOURCE_FILES
                .iter()
                .map(|source| (*source).to_owned())
                .collect::<Vec<_>>();
            let compile_input =
                compile_input_sha256(&source_dir, &source_files, &flags, &[]).unwrap();
            let spirv = [
                0x0723_0203_u32.to_le_bytes(),
                0x0001_0600_u32.to_le_bytes(),
                0_u32.to_le_bytes(),
                1_u32.to_le_bytes(),
                0_u32.to_le_bytes(),
            ]
            .concat();
            std::fs::write(shader_dir.join(FIXTURE_ARTIFACT), &spirv).unwrap();
            let manifest = json!({
                "schemaVersion": MANIFEST_SCHEMA_VERSION,
                "slangcVersion": "2026.12.2",
                "spirvFlags": flags,
                "artifacts": [{
                    "shader": FIXTURE_SHADER,
                    "source": FIXTURE_SOURCE,
                    "artifact": FIXTURE_ARTIFACT,
                    "defines": [],
                    "sourceFiles": source_files,
                    "compileInputSha256": compile_input.to_string(),
                    "spirvSha256": ShaderSha256::digest(&spirv).to_string(),
                }],
            });
            let fixture = Self {
                shader_dir,
                manifest,
                spirv,
            };
            fixture.write_manifest();
            fixture
        }

        fn write_manifest(&self) {
            let mut bytes = serde_json::to_vec_pretty(&self.manifest).unwrap();
            bytes.push(b'\n');
            std::fs::write(self.shader_dir.join(MANIFEST_NAME), bytes).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.shader_dir);
        }
    }

    #[test]
    fn strict_reader_binds_compiler_sources_flags_and_artifact() {
        let mut fixture = Fixture::new();
        let (first, bytes) = load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER).unwrap();
        assert_eq!(bytes, fixture.spirv);
        assert_eq!(first.shader(), FIXTURE_SHADER);
        assert_eq!(first.source(), FIXTURE_SOURCE);
        assert_eq!(first.artifact(), FIXTURE_ARTIFACT);
        assert_eq!(
            first
                .source_files()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            FIXTURE_SOURCE_FILES
        );
        assert_eq!(first.compiler_identity(), "2026.12.2");
        assert_eq!(
            first
                .spirv_flags()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            EXPECTED_SPIRV_FLAGS
        );
        assert!(first.defines().is_empty());
        assert_eq!(first.spirv_sha256(), ShaderSha256::digest(&bytes));
        assert_ne!(first.record_sha256().bytes(), [0; 32]);
        ShaderArtifactContract::new(
            FIXTURE_SHADER,
            FIXTURE_SOURCE,
            FIXTURE_ARTIFACT,
            FIXTURE_SOURCE_FILES,
            &[],
        )
        .verify(&first)
        .unwrap();
        assert!(matches!(
            ShaderArtifactContract::new(
                FIXTURE_SHADER,
                FIXTURE_SOURCE,
                FIXTURE_ARTIFACT,
                &[FIXTURE_SOURCE],
                &[],
            )
            .verify(&first),
            Err(ShaderArtifactError::ContractMismatch { .. })
        ));

        fixture.manifest["slangcVersion"] = json!("2026.12.3");
        fixture.write_manifest();
        let (second, _) = load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER).unwrap();
        assert_eq!(first.compile_input_sha256(), second.compile_input_sha256());
        assert_eq!(first.spirv_sha256(), second.spirv_sha256());
        assert_ne!(
            first.compiler_identity_sha256(),
            second.compiler_identity_sha256()
        );
        assert_ne!(first.record_sha256(), second.record_sha256());
    }

    #[test]
    fn strict_reader_rejects_malformed_schema_and_flag_drift() {
        let mut fixture = Fixture::new();
        fixture.manifest["unexpected"] = json!(true);
        fixture.write_manifest();
        assert!(matches!(
            load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER),
            Err(ShaderArtifactError::MalformedManifest { .. })
        ));

        fixture
            .manifest
            .as_object_mut()
            .unwrap()
            .remove("unexpected");
        fixture.manifest["spirvFlags"]
            .as_array_mut()
            .unwrap()
            .push(json!("-O3"));
        fixture.write_manifest();
        assert!(matches!(
            load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER),
            Err(ShaderArtifactError::UnexpectedSpirvFlags)
        ));
    }

    #[test]
    fn strict_reader_rejects_schema_compiler_and_record_drift() {
        let mut fixture = Fixture::new();
        fixture.manifest["schemaVersion"] = json!(MANIFEST_SCHEMA_VERSION + 1);
        fixture.write_manifest();
        assert!(matches!(
            load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER),
            Err(ShaderArtifactError::UnsupportedSchema { .. })
        ));

        fixture.manifest["schemaVersion"] = json!(MANIFEST_SCHEMA_VERSION);
        fixture.manifest["slangcVersion"] = json!("2026.12.2\n");
        fixture.write_manifest();
        assert!(matches!(
            load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER),
            Err(ShaderArtifactError::InvalidCompilerIdentity)
        ));

        fixture.manifest["slangcVersion"] = json!("2026.12.2");
        fixture.manifest["artifacts"][0]["artifact"] = json!("other.spv");
        fixture.write_manifest();
        assert!(matches!(
            load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER),
            Err(ShaderArtifactError::NoncanonicalRecord { .. })
        ));
    }

    #[test]
    fn strict_reader_rejects_source_and_artifact_drift() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.shader_dir.join(FIXTURE_ARTIFACT),
            [fixture.spirv.as_slice(), &[0, 0, 0, 0]].concat(),
        )
        .unwrap();
        assert!(matches!(
            load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER),
            Err(ShaderArtifactError::SpirvMismatch { .. })
        ));

        std::fs::write(fixture.shader_dir.join(FIXTURE_ARTIFACT), &fixture.spirv).unwrap();
        std::fs::write(
            fixture.shader_dir.join("source/spatial_numeric.slang"),
            b"const uint VALUE = 8;\n",
        )
        .unwrap();
        assert!(matches!(
            load_shader_artifact(&fixture.shader_dir, FIXTURE_SHADER),
            Err(ShaderArtifactError::CompileInputMismatch { .. })
        ));
    }

    #[test]
    fn compile_input_hash_matches_xtask_golden() {
        let fixture = Fixture::new();
        let source_files = FIXTURE_SOURCE_FILES
            .iter()
            .map(|source| (*source).to_owned())
            .collect::<Vec<_>>();
        let flags = EXPECTED_SPIRV_FLAGS
            .iter()
            .map(|flag| (*flag).to_owned())
            .collect::<Vec<_>>();
        let hash = compile_input_sha256(
            &fixture.shader_dir.join("source"),
            &source_files,
            &flags,
            &[],
        )
        .unwrap();
        assert_eq!(
            hash.to_string(),
            "122b26fd9511b87f07623958835354f11e37d34f89eed7f6496ec3e8804570d0"
        );
    }
}
