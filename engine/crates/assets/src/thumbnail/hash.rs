use crate::material::MaterialAsset;

use super::{ThumbnailContent, cache::THUMBNAIL_CACHE_VERSION};

/// The FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 1469598103934665603;
/// The FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 1099511628211;

/// An FNV-1a 64-bit accumulator: the content-hash fold over `u64` words + `f32` bits.
struct FnvHash(u64);

impl FnvHash {
    fn new() -> Self {
        Self(FNV_OFFSET)
    }

    fn mix(&mut self, v: u64) {
        self.0 ^= v;
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }

    fn mix_f(&mut self, f: f32) {
        self.mix(u64::from(f.to_bits()));
    }
}

/// A material thumbnail keys on its *resolved* state (a content hash of the resolved
/// params + texture uuids), not a stored catalog hash — editing a parent material reflows
/// every instance without touching the child `.smat`. Folded with the cache version.
pub(super) fn thumbnail_material_hash(m: &MaterialAsset) -> u64 {
    let mut h = FnvHash::new();
    h.mix(u64::from(THUMBNAIL_CACHE_VERSION));
    h.mix_f(m.base_color.x);
    h.mix_f(m.base_color.y);
    h.mix_f(m.base_color.z);
    h.mix_f(m.base_color.w);
    h.mix_f(m.metallic);
    h.mix_f(m.roughness);
    h.mix_f(m.emissive.x);
    h.mix_f(m.emissive.y);
    h.mix_f(m.emissive.z);
    h.mix_f(m.emissive_strength);
    h.mix_f(m.normal_strength);
    h.mix_f(m.alpha_cutoff);
    h.mix_f(m.height_scale);
    h.mix_f(m.uv_tiling.x);
    h.mix_f(m.uv_tiling.y);
    h.mix_f(m.uv_offset.x);
    h.mix_f(m.uv_offset.y);
    h.mix(m.albedo_texture.value());
    h.mix(m.orm_texture.value());
    h.mix(m.normal_texture.value());
    h.mix(m.emissive_texture.value());
    h.mix(m.height_texture.value());
    h.mix(u64::from(m.unlit));
    h.mix(u64::from(m.double_sided));
    for c in m.shader.bytes() {
        h.mix(u64::from(c));
    }
    for c in m.blend.bytes() {
        h.mix(u64::from(c));
    }
    for byte in crate::material::material_asset_to_text(m, 0).bytes() {
        h.mix(u64::from(byte));
    }
    h.0
}

/// The content hash of a resolved thumbnail job's inputs, mirroring the value scan/bake store on the
/// [`AssetEntry`](saffron_scene::AssetEntry). A single mesh/texture folds its chunk (or file) bytes
/// exactly as bake/scan does; a model folds its merged mesh bytes + node transforms + material
/// state. `0` when the bytes are unreadable (the entry stays uncacheable) or for a material (which
/// keys on resolved state).
pub(super) fn content_hash_from_content(content: &ThumbnailContent) -> u64 {
    match content {
        ThumbnailContent::Texture(src) => {
            if src.bytes.is_empty() {
                std::fs::read(&src.path)
                    .map(|b| crate::import::hash_bytes_fnv(&b))
                    .unwrap_or(0)
            } else {
                crate::import::hash_bytes_fnv(&src.bytes)
            }
        }
        ThumbnailContent::Mesh { path, bytes } => {
            if bytes.is_empty() {
                std::fs::read(path)
                    .map(|b| crate::import::hash_bytes_fnv(&b))
                    .unwrap_or(0)
            } else {
                crate::import::hash_bytes_fnv(bytes)
            }
        }
        ThumbnailContent::Model {
            meshes,
            materials,
            textures,
        } => {
            let mut h = FnvHash::new();
            h.mix(u64::from(THUMBNAIL_CACHE_VERSION));
            for chunk in meshes {
                h.mix(crate::import::hash_bytes_fnv(&chunk.bytes));
                for f in chunk.transform.to_cols_array() {
                    h.mix_f(f);
                }
            }
            for mat in materials {
                h.mix(thumbnail_material_hash(mat));
            }
            for t in textures {
                if !t.bytes.is_empty() {
                    h.mix(crate::import::hash_bytes_fnv(&t.bytes));
                }
            }
            h.0
        }
        ThumbnailContent::Material => 0,
    }
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;

    use super::*;

    #[test]
    fn material_hash_changes_with_resolved_params() {
        let mut a = crate::material::default_material_asset();
        let s1 = thumbnail_material_hash(&a);
        a.metallic = 0.5;
        let s2 = thumbnail_material_hash(&a);
        assert_ne!(
            s1, s2,
            "a param change retires the cached material thumbnail"
        );
        a.albedo_texture = Uuid(1234);
        let s3 = thumbnail_material_hash(&a);
        assert_ne!(s2, s3, "a texture id change moves the hash");
    }
}
