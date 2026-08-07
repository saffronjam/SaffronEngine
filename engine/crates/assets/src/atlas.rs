//! Deterministic atlas packing for a plant family's material slots.
//!
//! A generated family binds one material slot per element class — bark, leaf, flower — and each
//! arrives as its own image. Drawing them separately is a bind per class on geometry that is
//! otherwise one draw, so the cooker packs them into one atlas and rewrites the generated UVs into
//! the sub-rectangle each slot landed in.
//!
//! The packer is a skyline over shelves. Entries sort by descending height then by slot, so the
//! layout depends on neither a hash nor an iteration whim — it reaches cooked bytes, and a layout
//! that moved between runs would change every artifact hash. Every entry carries a gutter of
//! transparent texels: without one, bilinear filtering at a sub-rectangle's edge reaches into its
//! neighbour, which is the classic atlas bleed.

/// Transparent texels kept between packed entries and around the atlas border.
///
/// One texel is enough for bilinear, which reaches half a texel past an edge. Mipping reaches
/// further, so a family that mips its atlas needs the gutter to grow with the level count — the
/// packer takes the gutter rather than assuming it.
pub const DEFAULT_ATLAS_GUTTER: u32 = 4;

/// One slot's placement within a packed atlas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasPlacement {
    /// The material slot this rectangle carries.
    pub slot: u32,
    /// Left edge in texels.
    pub x: u32,
    /// Top edge in texels.
    pub y: u32,
    /// Width in texels, excluding the gutter.
    pub width: u32,
    /// Height in texels, excluding the gutter.
    pub height: u32,
}

impl AtlasPlacement {
    /// Whether this rectangle overlaps `other`, gutters excluded.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }

    /// Rewrites a `[0, 1]` UV authored against this slot's own image into atlas space.
    ///
    /// The UV is clamped into the rectangle first. A generated UV outside `[0, 1]` means a tiling
    /// intent, and tiling cannot survive packing — an atlas has a neighbour where the wrap would
    /// be. Clamping keeps the sample inside the slot that owns it rather than sampling whatever
    /// was packed next door.
    #[must_use]
    pub fn remap(&self, uv: [f32; 2], atlas_width: u32, atlas_height: u32) -> [f32; 2] {
        if atlas_width == 0 || atlas_height == 0 {
            return uv;
        }
        let clamped = [uv[0].clamp(0.0, 1.0), uv[1].clamp(0.0, 1.0)];
        [
            (self.x as f32 + clamped[0] * self.width as f32) / atlas_width as f32,
            (self.y as f32 + clamped[1] * self.height as f32) / atlas_height as f32,
        ]
    }
}

/// A packed atlas layout: the extent to allocate and where every slot landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtlasLayout {
    /// Atlas width in texels.
    pub width: u32,
    /// Atlas height in texels.
    pub height: u32,
    /// Placements in ascending slot order, so a caller can index by slot.
    pub placements: Vec<AtlasPlacement>,
    /// Gutter texels the layout was packed with.
    pub gutter: u32,
}

impl AtlasLayout {
    /// The placement of `slot`, if it was packed.
    #[must_use]
    pub fn placement(&self, slot: u32) -> Option<&AtlasPlacement> {
        self.placements
            .binary_search_by_key(&slot, |placement| placement.slot)
            .ok()
            .map(|index| &self.placements[index])
    }

    /// Texels the packed rectangles occupy, gutters excluded, over the atlas area.
    ///
    /// The measure a caller tunes `max_edge` against: a low fraction means the atlas is mostly
    /// gutter and padding, which costs memory and bandwidth for nothing.
    #[must_use]
    pub fn occupancy(&self) -> f64 {
        let area = f64::from(self.width) * f64::from(self.height);
        if area <= 0.0 {
            return 0.0;
        }
        let used: f64 = self
            .placements
            .iter()
            .map(|placement| f64::from(placement.width) * f64::from(placement.height))
            .sum();
        used / area
    }
}

/// Packs `entries` — `(slot, width, height)` — into the smallest power-of-two atlas that holds
/// them, up to `max_edge`.
///
/// Returns `None` when the entries cannot fit within `max_edge`, when an entry has a zero
/// dimension, or when a slot repeats. A caller that gets `None` has a real problem to report: a
/// silently dropped slot renders untextured, which reads as a material bug rather than a budget
/// one.
#[must_use]
pub fn pack_atlas(entries: &[(u32, u32, u32)], max_edge: u32, gutter: u32) -> Option<AtlasLayout> {
    if entries.is_empty() || max_edge == 0 {
        return None;
    }
    let mut sorted: Vec<(u32, u32, u32)> = entries.to_vec();
    // Tallest first packs shelves tightly; the slot breaks ties so the order is total and the
    // layout never depends on the caller's own ordering.
    sorted.sort_by_key(|(slot, width, height)| {
        (std::cmp::Reverse(*height), std::cmp::Reverse(*width), *slot)
    });
    if sorted.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return None;
    }
    if sorted
        .iter()
        .any(|(_, width, height)| *width == 0 || *height == 0)
    {
        return None;
    }

    let mut edge = 1_u32;
    loop {
        if let Some(placements) = shelf_pack(&sorted, edge, gutter) {
            let mut placements = placements;
            placements.sort_by_key(|placement| placement.slot);
            return Some(AtlasLayout {
                width: edge,
                height: edge,
                placements,
                gutter,
            });
        }
        if edge >= max_edge {
            return None;
        }
        edge = edge.saturating_mul(2).min(max_edge);
    }
}

/// Places every entry on shelves within a square of `edge` texels, or fails if one will not fit.
fn shelf_pack(sorted: &[(u32, u32, u32)], edge: u32, gutter: u32) -> Option<Vec<AtlasPlacement>> {
    let mut placements = Vec::with_capacity(sorted.len());
    let mut cursor_x = gutter;
    let mut cursor_y = gutter;
    let mut shelf_height = 0_u32;
    for (slot, width, height) in sorted {
        // The entry plus its trailing gutter must fit; the leading gutter is already in the cursor.
        let advance_x = width.checked_add(gutter)?;
        if cursor_x.checked_add(*width)? > edge.checked_sub(gutter)? {
            // New shelf: drop below the tallest entry on the current one.
            cursor_x = gutter;
            cursor_y = cursor_y.checked_add(shelf_height)?.checked_add(gutter)?;
            shelf_height = 0;
        }
        if cursor_y.checked_add(*height)? > edge.checked_sub(gutter)? {
            return None;
        }
        placements.push(AtlasPlacement {
            slot: *slot,
            x: cursor_x,
            y: cursor_y,
            width: *width,
            height: *height,
        });
        cursor_x = cursor_x.checked_add(advance_x)?;
        shelf_height = shelf_height.max(*height);
    }
    Some(placements)
}

/// One material slot's decoded image, as the generator consumes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilySlotImage {
    /// The material slot the family's geometry binds.
    pub slot: u32,
    /// Image width in texels.
    pub width: u32,
    /// Image height in texels.
    pub height: u32,
    /// Row-major RGBA8 texels.
    pub rgba: Vec<u8>,
}

/// A family's packed atlas: the layout, the composited level-0 image, and its
/// coverage-preserving mip chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyAtlas {
    /// Where each slot landed, and the atlas extent.
    pub layout: AtlasLayout,
    /// Encoding of the packed texels, which an upload has to match.
    pub format: saffron_vegetation::PlantTextureFormat,
    /// The full mip chain, level 0 first.
    pub levels: Vec<crate::CoverageMip>,
}

/// Packs a family's slot images into one atlas and builds its coverage-preserving mip chain.
///
/// The two halves have to happen together. Packing alone leaves a caller to composite, and a naive
/// composite writes transparent texels into the gutter — which is correct for alpha but wrong for
/// colour, because a transparent black gutter filters into the slot's edge as a dark fringe. The
/// gutter here carries the *edge texel's colour* at zero alpha, so filtering pulls in the right hue
/// and no coverage.
///
/// Mips are the alpha-area-preserving chain, not a box filter: a foliage cutout thinned by naive
/// downsampling disappears at distance, which is the whole reason that chain exists.
///
/// Returns `None` when the slots cannot be packed within `max_edge`, when a slot's pixel buffer
/// does not match its declared extent, or when the slot set is empty.
#[must_use]
pub fn generate_family_atlas(
    slots: &[FamilySlotImage],
    max_edge: u32,
    gutter: u32,
    reference_cutoff_bits: u16,
) -> Option<FamilyAtlas> {
    for slot in slots {
        if slot.rgba.len() != slot.width as usize * slot.height as usize * 4 {
            return None;
        }
    }
    let entries: Vec<(u32, u32, u32)> = slots
        .iter()
        .map(|slot| (slot.slot, slot.width, slot.height))
        .collect();
    let layout = pack_atlas(&entries, max_edge, gutter)?;

    let width = layout.width as usize;
    let height = layout.height as usize;
    let mut atlas = vec![0_u8; width * height * 4];
    for slot in slots {
        let placement = layout.placement(slot.slot)?;
        // The slot's own texels, then its gutter as edge colour at zero alpha. Sampling the
        // nearest edge texel rather than the corner keeps a gradient continuous across the seam.
        for y in 0..placement.height as i64 + i64::from(gutter) * 2 {
            for x in 0..placement.width as i64 + i64::from(gutter) * 2 {
                let atlas_x = i64::from(placement.x) - i64::from(gutter) + x;
                let atlas_y = i64::from(placement.y) - i64::from(gutter) + y;
                if atlas_x < 0 || atlas_y < 0 || atlas_x >= width as i64 || atlas_y >= height as i64
                {
                    continue;
                }
                let source_x = (x - i64::from(gutter)).clamp(0, i64::from(placement.width) - 1);
                let source_y = (y - i64::from(gutter)).clamp(0, i64::from(placement.height) - 1);
                let inside = x >= i64::from(gutter)
                    && y >= i64::from(gutter)
                    && x < i64::from(gutter) + i64::from(placement.width)
                    && y < i64::from(gutter) + i64::from(placement.height);
                let source = (source_y as usize * slot.width as usize + source_x as usize) * 4;
                let target = (atlas_y as usize * width + atlas_x as usize) * 4;
                atlas[target] = slot.rgba[source];
                atlas[target + 1] = slot.rgba[source + 1];
                atlas[target + 2] = slot.rgba[source + 2];
                atlas[target + 3] = if inside { slot.rgba[source + 3] } else { 0 };
            }
        }
    }

    Some(FamilyAtlas {
        levels: crate::coverage_preserving_mips(
            &atlas,
            layout.width,
            layout.height,
            reference_cutoff_bits,
        ),
        // The slot images composite as decoded, so the packed texels carry the same sRGB colour
        // encoding the slot textures were authored in.
        format: saffron_vegetation::PlantTextureFormat::Rgba8Srgb,
        layout,
    })
}

impl FamilyAtlas {
    /// The level-0 alpha plane, tightly packed — the input an opacity-micromap derivation reads.
    ///
    /// OMM classification asks whether a micro-triangle's UV footprint is provably above or below
    /// the alpha cutoff, which needs a min/max pyramid over exactly this plane. Deriving it from
    /// the packed atlas rather than from a slot's own image is what makes the answer match what
    /// the shader samples: after packing, a triangle's UVs address atlas space, and a pyramid over
    /// the unpacked image would answer about texels the GPU never reads.
    #[must_use]
    pub fn alpha_plane(&self) -> Vec<u8> {
        self.levels.first().map_or_else(Vec::new, |level| {
            level.rgba.iter().skip(3).step_by(4).copied().collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<(u32, u32, u32)> {
        vec![(0, 256, 256), (1, 128, 64), (2, 64, 128), (3, 32, 32)]
    }

    #[test]
    fn every_packed_rectangle_is_disjoint_and_inside_the_atlas() {
        // The one property an atlas must have. Two rectangles that overlap put one slot's texels
        // inside another's, which renders as the wrong material on a leaf and nothing else.
        let layout = pack_atlas(&entries(), 2_048, DEFAULT_ATLAS_GUTTER).expect("the set packs");
        for placement in &layout.placements {
            assert!(placement.x + placement.width <= layout.width);
            assert!(placement.y + placement.height <= layout.height);
        }
        for (index, placement) in layout.placements.iter().enumerate() {
            for other in &layout.placements[index + 1..] {
                assert!(
                    !placement.overlaps(other),
                    "slots {} and {} overlap",
                    placement.slot,
                    other.slot
                );
            }
        }
    }

    #[test]
    fn the_gutter_separates_every_pair() {
        // Bilinear filtering reaches past a sub-rectangle's edge, so touching rectangles bleed
        // into each other — a leaf with a sliver of bark along it, visible only sometimes.
        let gutter = DEFAULT_ATLAS_GUTTER;
        let layout = pack_atlas(&entries(), 2_048, gutter).expect("the set packs");
        for placement in &layout.placements {
            assert!(placement.x >= gutter, "the border gutter is missing");
            assert!(placement.y >= gutter);
            assert!(placement.x + placement.width + gutter <= layout.width);
            assert!(placement.y + placement.height + gutter <= layout.height);
        }
        for (index, placement) in layout.placements.iter().enumerate() {
            for other in &layout.placements[index + 1..] {
                let grown = AtlasPlacement {
                    x: placement.x.saturating_sub(gutter),
                    y: placement.y.saturating_sub(gutter),
                    width: placement.width + gutter * 2,
                    height: placement.height + gutter * 2,
                    ..*placement
                };
                assert!(
                    !grown.overlaps(other),
                    "slots {} and {} sit closer than the gutter",
                    placement.slot,
                    other.slot
                );
            }
        }
    }

    #[test]
    fn the_layout_does_not_depend_on_the_callers_order() {
        // The layout reaches cooked bytes. One that moved with the caller's iteration order would
        // change every artifact hash for no reason a person could see.
        let forward = pack_atlas(&entries(), 2_048, DEFAULT_ATLAS_GUTTER).expect("packs");
        let mut shuffled = entries();
        shuffled.reverse();
        let reversed = pack_atlas(&shuffled, 2_048, DEFAULT_ATLAS_GUTTER).expect("packs");
        assert_eq!(forward, reversed);
    }

    #[test]
    fn a_set_that_cannot_fit_is_refused_rather_than_dropped() {
        // A silently dropped slot renders untextured, which reads as a material bug rather than
        // the budget one it is.
        assert!(pack_atlas(&[(0, 512, 512)], 256, DEFAULT_ATLAS_GUTTER).is_none());
        assert!(pack_atlas(&[(0, 0, 64)], 2_048, DEFAULT_ATLAS_GUTTER).is_none());
        assert!(pack_atlas(&[(0, 64, 64), (0, 32, 32)], 2_048, 0).is_none());
        assert!(pack_atlas(&[], 2_048, DEFAULT_ATLAS_GUTTER).is_none());
    }

    #[test]
    fn a_remapped_uv_lands_inside_its_own_slot() {
        let layout = pack_atlas(&entries(), 2_048, DEFAULT_ATLAS_GUTTER).expect("packs");
        for placement in &layout.placements {
            for uv in [[0.0, 0.0], [1.0, 1.0], [0.5, 0.25], [-2.0, 3.0]] {
                let [u, v] = placement.remap(uv, layout.width, layout.height);
                let x = u * layout.width as f32;
                let y = v * layout.height as f32;
                assert!(
                    x >= placement.x as f32 - 1e-3
                        && x <= (placement.x + placement.width) as f32 + 1e-3,
                    "slot {} u {u} escaped its rectangle",
                    placement.slot
                );
                assert!(
                    y >= placement.y as f32 - 1e-3
                        && y <= (placement.y + placement.height) as f32 + 1e-3,
                    "slot {} v {v} escaped its rectangle",
                    placement.slot
                );
            }
        }
    }

    /// Two solid slots: one opaque red, one opaque blue, so a bleed shows as the wrong hue.
    fn slot_images() -> Vec<FamilySlotImage> {
        let solid = |slot: u32, colour: [u8; 4], edge: u32| FamilySlotImage {
            slot,
            width: edge,
            height: edge,
            rgba: colour
                .iter()
                .copied()
                .cycle()
                .take(edge as usize * edge as usize * 4)
                .collect(),
        };
        vec![
            solid(0, [255, 0, 0, 255], 32),
            solid(1, [0, 0, 255, 255], 32),
        ]
    }

    #[test]
    fn the_generated_atlas_carries_each_slot_and_a_full_mip_chain() {
        let atlas = generate_family_atlas(&slot_images(), 2_048, DEFAULT_ATLAS_GUTTER, 32_768)
            .expect("the family packs");
        let level0 = &atlas.levels[0];
        assert_eq!(level0.width, atlas.layout.width);
        assert_eq!(level0.height, atlas.layout.height);
        // A full chain reaches 1x1; a truncated one leaves the far LOD sampling a level that does
        // not exist.
        let smallest = atlas.levels.last().expect("a chain");
        assert_eq!((smallest.width, smallest.height), (1, 1));
        for slot in &slot_images() {
            let placement = atlas.layout.placement(slot.slot).expect("slot placed");
            let centre = ((placement.y + placement.height / 2) as usize * level0.width as usize
                + (placement.x + placement.width / 2) as usize)
                * 4;
            assert_eq!(&level0.rgba[centre..centre + 4], &slot.rgba[..4]);
        }
    }

    #[test]
    fn the_gutter_carries_edge_colour_at_zero_alpha() {
        // A transparent BLACK gutter is the naive composite, and it filters into the slot's edge
        // as a dark fringe — the artefact that makes packed foliage look dirty at distance. The
        // gutter must carry the edge texel's colour and no coverage.
        let atlas = generate_family_atlas(&slot_images(), 2_048, DEFAULT_ATLAS_GUTTER, 32_768)
            .expect("the family packs");
        let level0 = &atlas.levels[0];
        let placement = atlas.layout.placement(0).expect("slot placed");
        let y = placement.y + placement.height / 2;
        let x = placement.x - 1;
        let texel = (y as usize * level0.width as usize + x as usize) * 4;
        assert_eq!(level0.rgba[texel], 255, "the gutter keeps the edge hue");
        assert_eq!(level0.rgba[texel + 1], 0);
        assert_eq!(level0.rgba[texel + 2], 0);
        assert_eq!(level0.rgba[texel + 3], 0, "the gutter carries no coverage");
    }

    #[test]
    fn the_alpha_plane_matches_the_packed_atlas_the_shader_samples() {
        // The derivation input an opacity micromap reads. Taking it from a slot's own image would
        // answer about texels the GPU never reads: after packing, a triangle's UVs address atlas
        // space, and the gutter that separates two slots is part of what a footprint can cover.
        let atlas = generate_family_atlas(&slot_images(), 2_048, DEFAULT_ATLAS_GUTTER, 32_768)
            .expect("the family packs");
        let level0 = &atlas.levels[0];
        let plane = atlas.alpha_plane();
        assert_eq!(plane.len(), level0.width as usize * level0.height as usize);
        for (index, alpha) in plane.iter().enumerate() {
            assert_eq!(*alpha, level0.rgba[index * 4 + 3]);
        }
        // Inside a solid opaque slot the plane saturates; in the gutter it is empty. A derivation
        // over this plane can therefore prove both states, which is the whole point of the pyramid.
        let placement = atlas.layout.placement(0).expect("slot placed");
        let inside = (placement.y + placement.height / 2) as usize * level0.width as usize
            + (placement.x + placement.width / 2) as usize;
        let gutter = (placement.y + placement.height / 2) as usize * level0.width as usize
            + (placement.x - 1) as usize;
        assert_eq!(plane[inside], 255);
        assert_eq!(plane[gutter], 0);
    }

    #[test]
    fn the_generated_plane_drives_a_real_opacity_micromap_derivation() {
        // The RT/OMM derivation inputs come from the packed atlas: a derivation over a slot's
        // own image would classify texels the GPU never reads, because after packing a triangle's
        // UVs address atlas space.
        //
        // Two triangles cover a whole slot's rectangle, so the derivation has a footprint that is
        // entirely inside opaque texels and can prove it.
        let atlas = generate_family_atlas(&slot_images(), 2_048, DEFAULT_ATLAS_GUTTER, 32_768)
            .expect("the family packs");
        let placement = *atlas.layout.placement(0).expect("slot placed");
        let plane = atlas.alpha_plane();
        let corner =
            |u: f32, v: f32| placement.remap([u, v], atlas.layout.width, atlas.layout.height);
        let uvs = [
            corner(0.0, 0.0),
            corner(1.0, 0.0),
            corner(1.0, 1.0),
            corner(0.0, 1.0),
        ];
        let build = saffron_geometry::derive_opacity_micromap(
            &[0, 1, 2, 0, 2, 3],
            &uvs,
            saffron_geometry::CoverageSourcePlane {
                alpha: &plane,
                width: atlas.layout.width,
                height: atlas.layout.height,
                rule: saffron_geometry::CoverageRule {
                    always_covered_at_or_above: 0.5,
                    never_covered_at_or_below: 0.4,
                },
                base_alpha: 1.0,
            },
            &saffron_material::OpacityMicromapDerivation {
                enabled: true,
                max_subdivision: 4,
                transparent_threshold: saffron_spatial::UnitInterval::ZERO,
                opaque_threshold: saffron_spatial::UnitInterval::ONE,
            },
        );
        assert_eq!(build.indices.len(), 2, "one index per source triangle");
        assert!(
            build.classes.opaque > 0,
            "a footprint inside a solid slot must prove opaque"
        );
    }

    #[test]
    fn a_slot_whose_pixels_do_not_match_its_extent_is_refused() {
        // A short buffer read positionally would composite one slot's texels into another's
        // rectangle, which is the atlas bug that looks like a material mix-up.
        let mut slots = slot_images();
        slots[0].rgba.truncate(16);
        assert!(generate_family_atlas(&slots, 2_048, DEFAULT_ATLAS_GUTTER, 32_768).is_none());
        assert!(generate_family_atlas(&[], 2_048, DEFAULT_ATLAS_GUTTER, 32_768).is_none());
    }

    #[test]
    fn the_atlas_is_the_smallest_power_of_two_that_holds_the_set() {
        // Occupancy is what a caller tunes against; an atlas that jumped straight to `max_edge`
        // would pass every disjointness check and waste most of its memory.
        let layout =
            pack_atlas(&[(0, 64, 64), (1, 64, 64)], 2_048, DEFAULT_ATLAS_GUTTER).expect("packs");
        assert_eq!(layout.width, 256);
        assert!(pack_atlas(&[(0, 64, 64), (1, 64, 64)], 128, DEFAULT_ATLAS_GUTTER).is_none());
        assert!(layout.occupancy() > 0.12);
        assert_eq!(
            layout.placement(1).map(|placement| placement.width),
            Some(64)
        );
        assert!(layout.placement(9).is_none());
    }
}
