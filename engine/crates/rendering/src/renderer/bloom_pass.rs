use super::*;

impl Renderer {
    /// Acquires this frame's transient bloom mip chain (half-res first, halving each level) sized
    /// off the display-extent `published_extent`, and imports each level into `graph`. The live
    /// level count is `floor(log2(min(w, h))) - 3` clamped to `[1, MAX_BLOOM_MIPS]`; empty when
    /// the viewport is too small to pyramid or an allocation fails.
    pub(super) fn acquire_bloom_mips(
        &mut self,
        graph: &mut RenderGraph,
        frame: usize,
    ) -> Vec<BloomMip> {
        let published = self.views[self.active_view.index()].published_extent();
        let min_dim = published.width.min(published.height);
        if min_dim < 8 {
            return Vec::new();
        }
        let levels = ((min_dim as f32).log2().floor() as i32 - 3)
            .clamp(1, crate::descriptors::MAX_BLOOM_MIPS as i32) as usize;
        let mut mips = Vec::with_capacity(levels);
        for i in 0..levels {
            let extent = vk::Extent2D {
                width: (published.width >> (i + 1)).max(1),
                height: (published.height >> (i + 1)).max(1),
            };
            let desc = crate::resources::ImageDesc::color_2d(
                extent,
                crate::OFFSCREEN_COLOR_FORMAT,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
            );
            let (image, view) = match self.transient.acquire_image(
                frame,
                crate::transient::BLOOM_MIP_KEYS[i],
                &desc,
            ) {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::error!("bloom mip {i} acquire failed: {err}");
                    return Vec::new();
                }
            };
            let res = graph.import_image(
                image,
                view,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            mips.push(BloomMip { res, view, extent });
        }
        mips
    }

    /// Acquires the two ping-pong anamorphic streak buffers at `extent` (the bloom mip0 resolution)
    /// and imports each into `graph`. Returns empty (streak skipped) when anamorphic is off or an
    /// allocation fails, so the composite falls back to the white streak view (no added energy).
    pub(super) fn acquire_bloom_streak(
        &mut self,
        graph: &mut RenderGraph,
        frame: usize,
        extent: vk::Extent2D,
    ) -> Vec<BloomMip> {
        if !self.bloom_anamorphic_enabled {
            return Vec::new();
        }
        let mut buffers = Vec::with_capacity(crate::transient::BLOOM_STREAK_KEYS.len());
        for key in crate::transient::BLOOM_STREAK_KEYS {
            let desc = crate::resources::ImageDesc::color_2d(
                extent,
                crate::OFFSCREEN_COLOR_FORMAT,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
            );
            let (image, view) = match self.transient.acquire_image(frame, key, &desc) {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::error!("bloom streak '{key}' acquire failed: {err}");
                    return Vec::new();
                }
            };
            let res = graph.import_image(
                image,
                view,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            buffers.push(BloomMip { res, view, extent });
        }
        buffers
    }

    /// Appends the scene-linear bloom pyramid on `color`, in place, before the tonemap: a Karis-
    /// averaged 13-tap downsample chain (Karis only on the first step), a progressive 9-tap tent
    /// upsample-add back down to `mip0`, then an energy-conserving `lerp(hdr, bloom * tint,
    /// intensity)` composite into `color`. One PSO drives all four pass kinds through the push's
    /// `pass` / `karis` fields; the per-pass sets are rewritten for this frame slot first.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_bloom_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        color_view: vk::ImageView,
        frame: usize,
        mips: &[BloomMip],
        streak: &[BloomMip],
    ) {
        let Some(bloom) = &pipelines.bloom else {
            return;
        };
        const DOWN: [&str; crate::descriptors::MAX_BLOOM_MIPS] = [
            "bloom-downsample-0",
            "bloom-downsample-1",
            "bloom-downsample-2",
            "bloom-downsample-3",
            "bloom-downsample-4",
            "bloom-downsample-5",
            "bloom-downsample-6",
        ];
        const UP: [&str; crate::descriptors::MAX_BLOOM_MIPS] = [
            "bloom-upsample-0",
            "bloom-upsample-1",
            "bloom-upsample-2",
            "bloom-upsample-3",
            "bloom-upsample-4",
            "bloom-upsample-5",
            "bloom-upsample-6",
        ];
        let view = &self.views[self.active_view.index()];
        let published = view.published_extent();
        let levels = mips.len();
        // The streak buffer the composite adds — its last ping-pong stage when armed, else nothing.
        let streak_buffer = streak.last();

        // The source/target view pairs in graph order (downsamples, upsamples, streak, composite),
        // rewritten into this frame slot's sets before the passes reference them. Only the last
        // (composite) pass samples the dirt + streak; the earlier passes get the white fallback.
        let white = self.default_white.view();
        let mut pairs: Vec<(vk::ImageView, vk::ImageView)> = Vec::with_capacity(2 * levels + 3);
        for i in 0..levels {
            let src = if i == 0 { color_view } else { mips[i - 1].view };
            pairs.push((src, mips[i].view));
        }
        for j in (0..levels.saturating_sub(1)).rev() {
            pairs.push((mips[j + 1].view, mips[j].view));
        }
        // Streak ping-pong: mip0 → streak0, streak0 → streak1 (each a wider horizontal blur).
        for (s, buffer) in streak.iter().enumerate() {
            let src = if s == 0 {
                mips[0].view
            } else {
                streak[s - 1].view
            };
            pairs.push((src, buffer.view));
        }
        pairs.push((mips[0].view, color_view));
        let dirt_view = self.bloom_dirt_texture.as_ref().map_or(white, |t| t.view());
        let streak_view = streak_buffer.map_or(white, |b| b.view);
        view.write_bloom_sets(
            &self.device,
            &self.descriptors,
            frame,
            &pairs,
            crate::view_target::BloomCompositeBindings {
                dirt: dirt_view,
                streak: streak_view,
                fallback: white,
            },
        );

        let groups = |n: u32| n.div_ceil(8);
        let scatter = self.bloom_scatter;
        let mut pass_idx = 0usize;

        // Downsample: color → mip0 (Karis), then mip[i-1] → mip[i].
        for i in 0..levels {
            let src_res = if i == 0 { color } else { mips[i - 1].res };
            let push = crate::BloomPush {
                filter_radius: scatter,
                intensity: self.bloom_intensity,
                threshold: self.bloom_threshold,
                pass: 0,
                karis: u32::from(i == 0),
                ..crate::BloomPush::identity()
            };
            self.add_compute_pass(
                graph,
                DOWN[i],
                bloom,
                view.bloom_set(frame, pass_idx),
                &[
                    (src_res, RgUsage::SampledReadCompute),
                    (mips[i].res, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&push).to_vec()),
                groups(mips[i].extent.width),
                groups(mips[i].extent.height),
                1,
            );
            pass_idx += 1;
        }

        // Tent upsample-add: mip[j+1] → mip[j], coarse to fine. The added contribution carries the
        // per-mip tint for level `j` (identity when the stack is off or shorter than the pyramid).
        for j in (0..levels.saturating_sub(1)).rev() {
            let push = crate::BloomPush {
                filter_radius: scatter,
                intensity: self.bloom_intensity,
                threshold: self.bloom_threshold,
                pass: 1,
                mip_tint: self.bloom_mip_tint.get(j).copied().unwrap_or([1.0; 3]),
                ..crate::BloomPush::identity()
            };
            self.add_compute_pass(
                graph,
                UP[j],
                bloom,
                view.bloom_set(frame, pass_idx),
                &[
                    (mips[j + 1].res, RgUsage::SampledReadCompute),
                    (mips[j].res, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&push).to_vec()),
                groups(mips[j].extent.width),
                groups(mips[j].extent.height),
                1,
            );
            pass_idx += 1;
        }

        // Anamorphic streak ping-pong: a horizontally-squeezed blur widened across two passes.
        for (s, buffer) in streak.iter().enumerate() {
            let src_res = if s == 0 {
                mips[0].res
            } else {
                streak[s - 1].res
            };
            let push = crate::BloomPush {
                pass: 3,
                filter_radius: scatter,
                anamorphic_ratio: self.bloom_anamorphic_ratio,
                ..crate::BloomPush::identity()
            };
            self.add_compute_pass(
                graph,
                crate::transient::BLOOM_STREAK_KEYS[s],
                bloom,
                view.bloom_set(frame, pass_idx),
                &[
                    (src_res, RgUsage::SampledReadCompute),
                    (buffer.res, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&push).to_vec()),
                groups(buffer.extent.width),
                groups(buffer.extent.height),
                1,
            );
            pass_idx += 1;
        }

        // Energy-conserving composite: attenuate mip0 by the dirt mask, add the streak, then
        // lerp(hdr, bloom * tint, intensity) in place on color. When the streak is off, the streak
        // binding is the white fallback and `anamorphic_intensity` is 0, so no streak energy adds.
        let mut inputs: Vec<(RgResource, RgUsage)> = vec![
            (mips[0].res, RgUsage::SampledReadCompute),
            (color, RgUsage::StorageImageRwCompute),
        ];
        if let Some(buffer) = streak_buffer {
            inputs.push((buffer.res, RgUsage::SampledReadCompute));
        }
        let push = crate::BloomPush {
            tint: self.bloom_tint,
            filter_radius: scatter,
            intensity: self.bloom_intensity,
            threshold: self.bloom_threshold,
            pass: 2,
            dirt_intensity: self.bloom_dirt_intensity,
            dirt_tint: self.bloom_dirt_tint,
            anamorphic_intensity: if streak_buffer.is_some() {
                self.bloom_anamorphic_intensity
            } else {
                0.0
            },
            anamorphic_tint: self.bloom_anamorphic_tint,
            ..crate::BloomPush::identity()
        };
        self.add_compute_pass(
            graph,
            "bloom-composite",
            bloom,
            view.bloom_set(frame, pass_idx),
            &inputs,
            Some(bytemuck::bytes_of(&push).to_vec()),
            groups(published.width),
            groups(published.height),
            1,
        );
    }
}
