//! The project-file `renderSettings` block: the renderer's render-panel state as JSON.
//!
//! The save path serializes the AA mode, exposure, and the feature toggles; the load path
//! applies a saved block, leaving any missing field at its current value and applying the
//! RT toggles only where the device supports ray tracing (so a project authored on an RT
//! machine loads cleanly on a software one). One block, one schema — the project document
//! round-trips it unchanged through save/load.

use serde_json::{Value, json};

use crate::{ColorGrade, GradeRange, QualityTier, Renderer, TonemapMode};

/// The render-panel state, gathered from the renderer's getters or parsed from a saved
/// `renderSettings` block. A field of `None` in a parse means "absent → keep current".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderSettings {
    /// The AA mode name (`"off"` / `"fxaa"` / `"taa"` / `"msaaN"`).
    pub aa: Option<String>,
    /// The tonemap exposure in stops.
    pub exposure_ev: Option<f32>,
    /// The scene-linear color grade (canonical ASC-CDL SOP+Sat + white balance).
    pub color_grading: Option<ColorGrade>,
    /// The display-space creative LUT asset id (`0` = none). Re-resolved to its GPU table by the
    /// asset-aware project loader (the renderer has no catalog).
    pub creative_lut_texture: Option<u64>,
    /// The creative-look intensity in `[0, 1]`.
    pub creative_lut_intensity: Option<f32>,
    /// The pre-tonemap bloom pyramid enable flag.
    pub bloom_enabled: Option<bool>,
    /// The energy-conserving bloom composite weight.
    pub bloom_intensity: Option<f32>,
    /// The bloom tent-upsample scatter radius (UV units).
    pub bloom_scatter: Option<f32>,
    /// The bloom tint.
    pub bloom_tint: Option<[f32; 3]>,
    /// The bloom soft-knee prefilter threshold.
    pub bloom_threshold: Option<f32>,
    /// The lens-dirt mask asset id (`0` = none).
    pub bloom_dirt_texture: Option<u64>,
    /// The lens-dirt mix fraction.
    pub bloom_dirt_intensity: Option<f32>,
    /// The lens-dirt tint.
    pub bloom_dirt_tint: Option<[f32; 3]>,
    /// Whether the anamorphic streak runs.
    pub bloom_anamorphic_enabled: Option<bool>,
    /// The anamorphic horizontal squeeze.
    pub bloom_anamorphic_ratio: Option<f32>,
    /// The anamorphic streak tint.
    pub bloom_anamorphic_tint: Option<[f32; 3]>,
    /// The anamorphic streak add weight.
    pub bloom_anamorphic_intensity: Option<f32>,
    /// The per-upsample-step tint stack (empty when off).
    pub bloom_per_mip_tint: Option<Vec<[f32; 3]>>,
    /// Clustered forward lighting on.
    pub clustered: Option<bool>,
    /// The depth pre-pass on.
    pub depth_prepass: Option<bool>,
    /// Shadow maps on.
    pub shadows: Option<bool>,
    /// Image-based lighting on.
    pub ibl: Option<bool>,
    /// The render-quality tier name (`"low"`/`"medium"`/`"high"`/`"ultra"`/`"custom"`) — the single
    /// knob for the SSGI / GTAO / contact-shadow stack.
    pub quality: Option<String>,
    /// The tonemap operator name (`"reinhard"`/`"aces"`/`"agx"`/`"pbr-neutral"`).
    pub tonemap: Option<String>,
    /// Dynamic diffuse GI on.
    pub ddgi: Option<bool>,
    /// The Global Distance Field (the camera-centered cascade clipmap the DDGI trace + far-field
    /// cone march read as their distance oracle) on.
    pub gdf: Option<bool>,
    /// SDF distance-field AO occlusion of the analytic IBL on.
    pub sky_occlusion: Option<bool>,
    /// Ray-traced shadows on (applied only on RT hardware).
    pub rt_shadows: Option<bool>,
    /// ReSTIR DI on (applied only on RT hardware).
    pub restir: Option<bool>,
}

/// Serializes a fully-populated [`RenderSettings`] (every field `Some`) to the project
/// `renderSettings` block. The pure half of [`Renderer::render_settings_to_json`], so the
/// frozen key schema is unit-testable without a device.
fn settings_to_json(s: &RenderSettings) -> Value {
    json!({
        "aa": s.aa,
        "exposureEv": s.exposure_ev,
        "colorGrading": s.color_grading.map(color_grading_to_json),
        "creativeLutTexture": s.creative_lut_texture,
        "creativeLutIntensity": s.creative_lut_intensity,
        "bloomEnabled": s.bloom_enabled,
        "bloomIntensity": s.bloom_intensity,
        "bloomScatter": s.bloom_scatter,
        "bloomTint": s.bloom_tint,
        "bloomThreshold": s.bloom_threshold,
        "bloomDirtTexture": s.bloom_dirt_texture,
        "bloomDirtIntensity": s.bloom_dirt_intensity,
        "bloomDirtTint": s.bloom_dirt_tint,
        "bloomAnamorphicEnabled": s.bloom_anamorphic_enabled,
        "bloomAnamorphicRatio": s.bloom_anamorphic_ratio,
        "bloomAnamorphicTint": s.bloom_anamorphic_tint,
        "bloomAnamorphicIntensity": s.bloom_anamorphic_intensity,
        "bloomPerMipTint": s.bloom_per_mip_tint,
        "clustered": s.clustered,
        "depthPrepass": s.depth_prepass,
        "shadows": s.shadows,
        "ibl": s.ibl,
        "quality": s.quality,
        "tonemap": s.tonemap,
        "ddgi": s.ddgi,
        "gdf": s.gdf,
        "skyOcclusion": s.sky_occlusion,
        "rtShadows": s.rt_shadows,
        "restir": s.restir,
    })
}

/// One masked range as its persisted `{ slope, offset, power, saturation, contrast }` block.
fn grade_range_to_json(r: GradeRange) -> Value {
    json!({
        "slope": r.slope,
        "offset": r.offset,
        "power": r.power,
        "saturation": r.saturation,
        "contrast": r.contrast,
    })
}

/// The canonical ASC-CDL SOP+Sat block a grade round-trips as (also the `.cdl`/`.ccc` interchange
/// form): white-balance Temp/Tint, contrast-around-pivot, saturation, and slope/offset/power, plus
/// the three masked ranges, the range knobs, the row-major channel mixer, and split-toning.
fn color_grading_to_json(g: ColorGrade) -> Value {
    json!({
        "temperature": g.temperature,
        "tint": g.tint,
        "contrast": g.contrast,
        "pivot": g.pivot,
        "saturation": g.saturation,
        "slope": g.slope,
        "offset": g.offset,
        "power": g.power,
        "shadows": grade_range_to_json(g.shadows),
        "midtones": grade_range_to_json(g.midtones),
        "highlights": grade_range_to_json(g.highlights),
        "shadowsMax": g.shadows_max,
        "highlightsMin": g.highlights_min,
        "channelMixer": g.channel_mixer,
        "splitTone": {
            "shadow": g.split_shadow,
            "highlight": g.split_highlight,
            "balance": g.split_balance,
        },
    })
}

/// Parses a fixed-length `f32` array; `None` on a wrong length or a non-numeric element.
fn parse_f32_array<const N: usize>(value: &Value) -> Option<[f32; N]> {
    let a = value.as_array()?;
    if a.len() != N {
        return None;
    }
    let mut out = [0.0f32; N];
    for (slot, item) in out.iter_mut().zip(a) {
        *slot = item.as_f64()? as f32;
    }
    Some(out)
}

/// Parses a `{ slope, offset, power, saturation, contrast }` range block; a missing key keeps its
/// neutral default, so a partial block still yields a valid range.
fn parse_grade_range(value: &Value) -> Option<GradeRange> {
    let obj = value.as_object()?;
    let mut range = GradeRange::default();
    if let Some(v) = obj.get("slope").and_then(parse_f32_array::<3>) {
        range.slope = v;
    }
    if let Some(v) = obj.get("offset").and_then(parse_f32_array::<3>) {
        range.offset = v;
    }
    if let Some(v) = obj.get("power").and_then(parse_f32_array::<3>) {
        range.power = v;
    }
    if let Some(v) = obj.get("saturation").and_then(Value::as_f64) {
        range.saturation = v as f32;
    }
    if let Some(v) = obj.get("contrast").and_then(Value::as_f64) {
        range.contrast = v as f32;
    }
    Some(range)
}

/// Parses a `colorGrading` block into a [`ColorGrade`]; a missing scalar keeps its neutral default,
/// so a partial block still yields a valid grade.
fn parse_color_grading(value: &Value) -> Option<ColorGrade> {
    let obj = value.as_object()?;
    let mut grade = ColorGrade::default();
    let f = |key: &str| obj.get(key).and_then(Value::as_f64).map(|v| v as f32);
    let vec3 = |key: &str| obj.get(key).and_then(parse_f32_array::<3>);
    if let Some(v) = f("temperature") {
        grade.temperature = v;
    }
    if let Some(v) = f("tint") {
        grade.tint = v;
    }
    if let Some(v) = f("contrast") {
        grade.contrast = v;
    }
    if let Some(v) = f("pivot") {
        grade.pivot = v;
    }
    if let Some(v) = f("saturation") {
        grade.saturation = v;
    }
    if let Some(v) = vec3("slope") {
        grade.slope = v;
    }
    if let Some(v) = vec3("offset") {
        grade.offset = v;
    }
    if let Some(v) = vec3("power") {
        grade.power = v;
    }
    if let Some(v) = obj.get("shadows").and_then(parse_grade_range) {
        grade.shadows = v;
    }
    if let Some(v) = obj.get("midtones").and_then(parse_grade_range) {
        grade.midtones = v;
    }
    if let Some(v) = obj.get("highlights").and_then(parse_grade_range) {
        grade.highlights = v;
    }
    if let Some(v) = f("shadowsMax") {
        grade.shadows_max = v;
    }
    if let Some(v) = f("highlightsMin") {
        grade.highlights_min = v;
    }
    if let Some(v) = obj.get("channelMixer").and_then(parse_f32_array::<9>) {
        grade.channel_mixer = v;
    }
    if let Some(split) = obj.get("splitTone").and_then(Value::as_object) {
        if let Some(v) = split.get("shadow").and_then(parse_f32_array::<3>) {
            grade.split_shadow = v;
        }
        if let Some(v) = split.get("highlight").and_then(parse_f32_array::<3>) {
            grade.split_highlight = v;
        }
        if let Some(v) = split.get("balance").and_then(Value::as_f64) {
            grade.split_balance = v as f32;
        }
    }
    Some(grade)
}

/// Parses a saved `renderSettings` block into a [`RenderSettings`] patch: a missing or
/// wrong-typed field stays `None` (→ keep current). A non-object value parses to an
/// all-`None` patch (a no-op). The pure half of [`Renderer::apply_render_settings`].
fn parse_render_settings(settings: &Value) -> RenderSettings {
    let mut patch = RenderSettings::default();
    let Some(obj) = settings.as_object() else {
        return patch;
    };
    patch.aa = obj.get("aa").and_then(Value::as_str).map(str::to_owned);
    patch.exposure_ev = obj
        .get("exposureEv")
        .and_then(Value::as_f64)
        .map(|v| v as f32);
    patch.color_grading = obj.get("colorGrading").and_then(parse_color_grading);
    patch.creative_lut_texture = obj.get("creativeLutTexture").and_then(Value::as_u64);
    let b = |key: &str| obj.get(key).and_then(Value::as_bool);
    let f = |key: &str| obj.get(key).and_then(Value::as_f64).map(|v| v as f32);
    let color3 = |value: &Value| -> Option<[f32; 3]> {
        let a = value.as_array()?;
        if a.len() != 3 {
            return None;
        }
        let mut out = [0.0f32; 3];
        for (slot, item) in out.iter_mut().zip(a) {
            *slot = item.as_f64()? as f32;
        }
        Some(out)
    };
    let color3_key = |key: &str| obj.get(key).and_then(color3);
    patch.creative_lut_intensity = f("creativeLutIntensity");
    patch.bloom_enabled = b("bloomEnabled");
    patch.bloom_intensity = f("bloomIntensity");
    patch.bloom_scatter = f("bloomScatter");
    patch.bloom_threshold = f("bloomThreshold");
    patch.bloom_tint = color3_key("bloomTint");
    patch.bloom_dirt_texture = obj.get("bloomDirtTexture").and_then(Value::as_u64);
    patch.bloom_dirt_intensity = f("bloomDirtIntensity");
    patch.bloom_dirt_tint = color3_key("bloomDirtTint");
    patch.bloom_anamorphic_enabled = b("bloomAnamorphicEnabled");
    patch.bloom_anamorphic_ratio = f("bloomAnamorphicRatio");
    patch.bloom_anamorphic_tint = color3_key("bloomAnamorphicTint");
    patch.bloom_anamorphic_intensity = f("bloomAnamorphicIntensity");
    patch.bloom_per_mip_tint = obj.get("bloomPerMipTint").and_then(|v| {
        v.as_array()?
            .iter()
            .map(&color3)
            .collect::<Option<Vec<[f32; 3]>>>()
    });
    patch.clustered = b("clustered");
    patch.depth_prepass = b("depthPrepass");
    patch.shadows = b("shadows");
    patch.ibl = b("ibl");
    patch.quality = obj
        .get("quality")
        .and_then(Value::as_str)
        .map(str::to_owned);
    patch.tonemap = obj
        .get("tonemap")
        .and_then(Value::as_str)
        .map(str::to_owned);
    patch.ddgi = b("ddgi");
    patch.gdf = b("gdf");
    patch.sky_occlusion = b("skyOcclusion");
    patch.rt_shadows = b("rtShadows");
    patch.restir = b("restir");
    patch
}

impl Renderer {
    /// Serializes the renderer's render-panel settings as the project-file
    /// `renderSettings` block.
    pub fn render_settings_to_json(&self) -> Value {
        settings_to_json(&RenderSettings {
            aa: Some(self.aa_mode()),
            exposure_ev: Some(self.exposure_ev()),
            color_grading: Some(self.color_grading()),
            creative_lut_texture: Some(self.creative_lut().map_or(0, |(id, _, _)| id)),
            creative_lut_intensity: Some(self.creative_lut().map_or(0.0, |(_, _, i)| i)),
            bloom_enabled: Some(self.bloom_enabled()),
            bloom_intensity: Some(self.bloom_intensity()),
            bloom_scatter: Some(self.bloom_scatter()),
            bloom_tint: Some(self.bloom_tint()),
            bloom_threshold: Some(self.bloom_threshold()),
            bloom_dirt_texture: Some(self.bloom_dirt_texture()),
            bloom_dirt_intensity: Some(self.bloom_dirt_intensity()),
            bloom_dirt_tint: Some(self.bloom_dirt_tint()),
            bloom_anamorphic_enabled: Some(self.bloom_anamorphic_enabled()),
            bloom_anamorphic_ratio: Some(self.bloom_anamorphic_ratio()),
            bloom_anamorphic_tint: Some(self.bloom_anamorphic_tint()),
            bloom_anamorphic_intensity: Some(self.bloom_anamorphic_intensity()),
            bloom_per_mip_tint: Some(self.bloom_mip_tint()),
            clustered: Some(self.clustered_enabled()),
            depth_prepass: Some(self.depth_prepass_enabled()),
            shadows: Some(self.shadows_enabled()),
            ibl: Some(self.ibl_enabled()),
            quality: Some(self.render_quality().tier.as_str().to_owned()),
            tonemap: Some(self.tonemap_mode().as_str().to_owned()),
            ddgi: Some(self.ddgi_enabled()),
            gdf: Some(self.gdf_enabled()),
            sky_occlusion: Some(self.sky_occlusion_enabled()),
            rt_shadows: Some(self.rt_shadows_enabled()),
            restir: Some(self.restir_enabled()),
        })
    }

    /// Applies a saved `renderSettings` block: a missing field keeps the current value,
    /// and the RT toggles apply only where the device supports ray tracing. A non-object
    /// value (or a wrong-typed field) is ignored field-by-field, so a malformed block
    /// degrades to "keep current".
    pub fn apply_render_settings(&mut self, settings: &Value) {
        let patch = parse_render_settings(settings);
        if let Some(aa) = &patch.aa {
            // The AA mode setter idles + rebuilds the active view's AA targets; a bad name
            // falls back to "off" inside the setter.
            let _ = self.set_aa_mode(aa);
        }
        if let Some(ev) = patch.exposure_ev {
            self.set_exposure(ev);
        }
        if let Some(grade) = patch.color_grading {
            self.set_color_grading(grade);
        }
        // The creative-LUT intensity applies through the renderer; the LUT *asset* is rebound by the
        // asset-aware project loader (the renderer has no catalog), matching the lens-dirt mask. A
        // saved intensity with the current asset (or none) updates the grade UBO without a rebind.
        if let Some(intensity) = patch.creative_lut_intensity {
            // The asset id here equals the current one, so `set_creative_lut_texture` updates only the
            // intensity (the `lut` arg is unused on the no-change path); the loader rebinds the asset.
            let id = self.creative_lut().map_or(0, |(id, _, _)| id);
            self.set_creative_lut_texture(id, None, intensity);
        }
        // Bloom is one setter over five fields; a missing field keeps the current value.
        if patch.bloom_enabled.is_some()
            || patch.bloom_intensity.is_some()
            || patch.bloom_scatter.is_some()
            || patch.bloom_tint.is_some()
            || patch.bloom_threshold.is_some()
        {
            self.set_bloom(
                patch.bloom_enabled.unwrap_or_else(|| self.bloom_enabled()),
                patch
                    .bloom_intensity
                    .unwrap_or_else(|| self.bloom_intensity()),
                patch.bloom_scatter.unwrap_or_else(|| self.bloom_scatter()),
                patch.bloom_tint.unwrap_or_else(|| self.bloom_tint()),
                patch
                    .bloom_threshold
                    .unwrap_or_else(|| self.bloom_threshold()),
            );
        }
        // The lens-dirt mix + tint and the anamorphic streak apply through the renderer alone; the
        // dirt mask asset is rebound by the asset-aware project loader (the renderer has no catalog).
        if patch.bloom_dirt_intensity.is_some() || patch.bloom_dirt_tint.is_some() {
            self.set_bloom_dirt_params(
                patch
                    .bloom_dirt_intensity
                    .unwrap_or_else(|| self.bloom_dirt_intensity()),
                patch
                    .bloom_dirt_tint
                    .unwrap_or_else(|| self.bloom_dirt_tint()),
            );
        }
        if patch.bloom_anamorphic_enabled.is_some()
            || patch.bloom_anamorphic_ratio.is_some()
            || patch.bloom_anamorphic_tint.is_some()
            || patch.bloom_anamorphic_intensity.is_some()
        {
            self.set_bloom_anamorphic(
                patch
                    .bloom_anamorphic_enabled
                    .unwrap_or_else(|| self.bloom_anamorphic_enabled()),
                patch
                    .bloom_anamorphic_ratio
                    .unwrap_or_else(|| self.bloom_anamorphic_ratio()),
                patch
                    .bloom_anamorphic_tint
                    .unwrap_or_else(|| self.bloom_anamorphic_tint()),
                patch
                    .bloom_anamorphic_intensity
                    .unwrap_or_else(|| self.bloom_anamorphic_intensity()),
            );
        }
        if let Some(stack) = patch.bloom_per_mip_tint {
            self.set_bloom_mip_tint(stack);
        }
        if let Some(v) = patch.clustered {
            self.set_clustered(v);
        }
        if let Some(v) = patch.depth_prepass {
            self.set_depth_prepass(v);
        }
        if let Some(v) = patch.shadows {
            self.set_shadows(v);
        }
        if let Some(v) = patch.ibl {
            self.set_ibl(v);
        }
        if let Some(name) = &patch.quality
            && let Some(tier) = QualityTier::from_name(name)
        {
            self.set_render_quality(tier.resolve());
        }
        if let Some(name) = &patch.tonemap
            && let Some(mode) = TonemapMode::from_name(name)
        {
            self.set_tonemap_mode(mode);
        }
        if let Some(v) = patch.ddgi {
            self.set_ddgi(v);
        }
        if let Some(v) = patch.gdf {
            self.set_gdf(v);
        }
        if let Some(v) = patch.sky_occlusion {
            self.set_sky_occlusion(v);
        }
        if self.rt_supported() {
            if let Some(v) = patch.rt_shadows {
                self.set_rt_shadows(v);
            }
            if let Some(v) = patch.restir {
                self.set_restir(v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The serialized block carries every render-panel field with the frozen project keys
    /// (the schema the editor's render panel reads). Pure logic — no device needed (a
    /// headless `Renderer::new` crashes lavapipe's WSI).
    #[test]
    fn render_settings_block_has_the_frozen_keys() {
        let settings = RenderSettings {
            aa: Some("msaa4".to_owned()),
            exposure_ev: Some(0.5),
            color_grading: Some(ColorGrade {
                temperature: 5000.0,
                tint: 0.1,
                contrast: 1.2,
                pivot: 0.18,
                saturation: 0.9,
                slope: [1.0, 0.95, 0.9],
                offset: [0.0, 0.01, 0.02],
                power: [1.0, 1.05, 1.1],
                ..ColorGrade::default()
            }),
            creative_lut_texture: Some(7),
            creative_lut_intensity: Some(0.8),
            bloom_enabled: Some(true),
            bloom_intensity: Some(0.06),
            bloom_scatter: Some(0.004),
            bloom_tint: Some([1.0, 0.9, 0.8]),
            bloom_threshold: Some(0.0),
            bloom_dirt_texture: Some(42),
            bloom_dirt_intensity: Some(0.5),
            bloom_dirt_tint: Some([1.0, 0.95, 0.9]),
            bloom_anamorphic_enabled: Some(true),
            bloom_anamorphic_ratio: Some(2.0),
            bloom_anamorphic_tint: Some([0.6, 0.8, 1.0]),
            bloom_anamorphic_intensity: Some(0.3),
            bloom_per_mip_tint: Some(vec![[1.0, 0.5, 0.5], [0.5, 0.5, 1.0]]),
            clustered: Some(true),
            depth_prepass: Some(false),
            shadows: Some(true),
            ibl: Some(true),
            quality: Some("medium".to_owned()),
            tonemap: Some("aces".to_owned()),
            ddgi: Some(true),
            gdf: Some(true),
            sky_occlusion: Some(true),
            rt_shadows: Some(false),
            restir: Some(false),
        };
        let block = settings_to_json(&settings);
        let obj = block.as_object().expect("renderSettings is an object");
        for key in [
            "aa",
            "exposureEv",
            "colorGrading",
            "creativeLutTexture",
            "creativeLutIntensity",
            "bloomEnabled",
            "bloomIntensity",
            "bloomScatter",
            "bloomTint",
            "bloomThreshold",
            "bloomDirtTexture",
            "bloomDirtIntensity",
            "bloomDirtTint",
            "bloomAnamorphicEnabled",
            "bloomAnamorphicRatio",
            "bloomAnamorphicTint",
            "bloomAnamorphicIntensity",
            "bloomPerMipTint",
            "clustered",
            "depthPrepass",
            "shadows",
            "ibl",
            "quality",
            "tonemap",
            "ddgi",
            "gdf",
            "skyOcclusion",
            "rtShadows",
            "restir",
        ] {
            assert!(obj.contains_key(key), "renderSettings carries '{key}'");
        }
        assert_eq!(obj["aa"], json!("msaa4"), "aa is the mode name");
        assert_eq!(obj["exposureEv"].as_f64().unwrap(), 0.5);
        assert_eq!(obj["clustered"], json!(true));
        assert_eq!(obj["shadows"], json!(true));
        let grade = obj["colorGrading"]
            .as_object()
            .expect("colorGrading is an object");
        for key in [
            "temperature",
            "tint",
            "contrast",
            "pivot",
            "saturation",
            "slope",
            "offset",
            "power",
            "shadows",
            "midtones",
            "highlights",
            "shadowsMax",
            "highlightsMin",
            "channelMixer",
            "splitTone",
        ] {
            assert!(grade.contains_key(key), "colorGrading carries '{key}'");
        }
    }

    /// Parsing a saved block then re-serializing reproduces it — the project save/load
    /// round-trip over the pure serde halves (the `Renderer` wrappers add only the
    /// getter/setter plumbing the device-gated render tests already exercise).
    #[test]
    fn parse_then_serialize_round_trips_every_field() {
        let saved = json!({
            "aa": "fxaa",
            "exposureEv": 1.5,
            "colorGrading": {
                "temperature": 5000.0,
                "tint": 0.5,
                "contrast": 1.5,
                "pivot": 0.25,
                "saturation": 0.5,
                "slope": [1.0, 0.5, 0.25],
                "offset": [0.0, 0.25, 0.5],
                "power": [1.0, 1.5, 2.0],
                "shadows": {
                    "slope": [1.5, 1.0, 0.5],
                    "offset": [0.25, 0.0, -0.25],
                    "power": [1.0, 1.0, 1.25],
                    "saturation": 1.5,
                    "contrast": 0.5,
                },
                "midtones": {
                    "slope": [1.0, 1.0, 1.0],
                    "offset": [0.0, 0.0, 0.0],
                    "power": [0.5, 1.0, 1.5],
                    "saturation": 1.0,
                    "contrast": 1.0,
                },
                "highlights": {
                    "slope": [0.5, 1.0, 1.5],
                    "offset": [0.0, 0.0, 0.25],
                    "power": [1.0, 1.0, 1.0],
                    "saturation": 0.5,
                    "contrast": 1.25,
                },
                "shadowsMax": 0.125,
                "highlightsMin": 0.5,
                "channelMixer": [1.0, 0.25, 0.0, 0.0, 1.0, 0.0, 0.0, 0.125, 1.0],
                "splitTone": {
                    "shadow": [0.25, 0.5, 0.75],
                    "highlight": [0.75, 0.5, 0.25],
                    "balance": 0.125,
                },
            },
            "creativeLutTexture": 7,
            "creativeLutIntensity": 0.5,
            "bloomEnabled": true,
            "bloomIntensity": 0.0625,
            "bloomScatter": 0.00390625,
            "bloomTint": [1.0, 0.5, 0.25],
            "bloomThreshold": 0.0,
            "bloomDirtTexture": 42,
            "bloomDirtIntensity": 0.5,
            "bloomDirtTint": [1.0, 0.5, 0.25],
            "bloomAnamorphicEnabled": true,
            "bloomAnamorphicRatio": 2.0,
            "bloomAnamorphicTint": [0.5, 0.75, 1.0],
            "bloomAnamorphicIntensity": 0.25,
            "bloomPerMipTint": [[1.0, 0.5, 0.5], [0.5, 0.5, 1.0]],
            "clustered": false,
            "depthPrepass": true,
            "shadows": false,
            "ibl": false,
            "quality": "ultra",
            "tonemap": "agx",
            "ddgi": true,
            "gdf": false,
            "skyOcclusion": false,
            "rtShadows": true,
            "restir": false,
        });
        let patch = parse_render_settings(&saved);
        // Every field parsed.
        assert_eq!(patch.aa.as_deref(), Some("fxaa"));
        assert_eq!(patch.exposure_ev, Some(1.5));
        assert_eq!(
            patch.color_grading,
            Some(ColorGrade {
                temperature: 5000.0,
                tint: 0.5,
                contrast: 1.5,
                pivot: 0.25,
                saturation: 0.5,
                slope: [1.0, 0.5, 0.25],
                offset: [0.0, 0.25, 0.5],
                power: [1.0, 1.5, 2.0],
                shadows: GradeRange {
                    slope: [1.5, 1.0, 0.5],
                    offset: [0.25, 0.0, -0.25],
                    power: [1.0, 1.0, 1.25],
                    saturation: 1.5,
                    contrast: 0.5,
                },
                midtones: GradeRange {
                    slope: [1.0, 1.0, 1.0],
                    offset: [0.0, 0.0, 0.0],
                    power: [0.5, 1.0, 1.5],
                    saturation: 1.0,
                    contrast: 1.0,
                },
                highlights: GradeRange {
                    slope: [0.5, 1.0, 1.5],
                    offset: [0.0, 0.0, 0.25],
                    power: [1.0, 1.0, 1.0],
                    saturation: 0.5,
                    contrast: 1.25,
                },
                shadows_max: 0.125,
                highlights_min: 0.5,
                channel_mixer: [1.0, 0.25, 0.0, 0.0, 1.0, 0.0, 0.0, 0.125, 1.0],
                split_shadow: [0.25, 0.5, 0.75],
                split_highlight: [0.75, 0.5, 0.25],
                split_balance: 0.125,
            })
        );
        assert_eq!(patch.creative_lut_texture, Some(7));
        assert_eq!(patch.creative_lut_intensity, Some(0.5));
        assert_eq!(patch.bloom_enabled, Some(true));
        assert_eq!(patch.bloom_intensity, Some(0.0625));
        assert_eq!(patch.bloom_scatter, Some(0.00390625));
        assert_eq!(patch.bloom_tint, Some([1.0, 0.5, 0.25]));
        assert_eq!(patch.bloom_threshold, Some(0.0));
        assert_eq!(patch.bloom_dirt_texture, Some(42));
        assert_eq!(patch.bloom_dirt_intensity, Some(0.5));
        assert_eq!(patch.bloom_dirt_tint, Some([1.0, 0.5, 0.25]));
        assert_eq!(patch.bloom_anamorphic_enabled, Some(true));
        assert_eq!(patch.bloom_anamorphic_ratio, Some(2.0));
        assert_eq!(patch.bloom_anamorphic_tint, Some([0.5, 0.75, 1.0]));
        assert_eq!(patch.bloom_anamorphic_intensity, Some(0.25));
        assert_eq!(
            patch.bloom_per_mip_tint,
            Some(vec![[1.0, 0.5, 0.5], [0.5, 0.5, 1.0]])
        );
        assert_eq!(patch.clustered, Some(false));
        assert_eq!(patch.depth_prepass, Some(true));
        assert_eq!(patch.shadows, Some(false));
        assert_eq!(patch.ibl, Some(false));
        assert_eq!(patch.quality.as_deref(), Some("ultra"));
        assert_eq!(patch.tonemap.as_deref(), Some("agx"));
        assert_eq!(patch.ddgi, Some(true));
        assert_eq!(patch.gdf, Some(false));
        assert_eq!(patch.sky_occlusion, Some(false));
        assert_eq!(patch.rt_shadows, Some(true));
        assert_eq!(patch.restir, Some(false));

        // Re-serializing the fully-populated patch reproduces the saved block.
        assert_eq!(settings_to_json(&patch), saved);
    }

    /// A missing field parses to `None` (→ keep current); a non-object block is an
    /// all-`None` patch (a no-op); a wrong-typed field is ignored field-by-field.
    #[test]
    fn missing_and_malformed_fields_parse_to_none() {
        // A block touching only `quality` leaves every other field `None`.
        let patch = parse_render_settings(&json!({ "quality": "low" }));
        assert_eq!(patch.quality.as_deref(), Some("low"), "quality parsed");
        assert_eq!(patch.shadows, None, "absent → keep current");
        assert_eq!(patch.aa, None);

        // A non-object block parses to an all-`None` patch.
        assert_eq!(
            parse_render_settings(&json!("not an object")),
            RenderSettings::default(),
            "a non-object block is a no-op"
        );

        // A wrong-typed field is ignored (a string where a bool is expected).
        let patch = parse_render_settings(&json!({ "shadows": "yes" }));
        assert_eq!(
            patch.shadows, None,
            "a string for a boolean field is ignored"
        );
    }
}
