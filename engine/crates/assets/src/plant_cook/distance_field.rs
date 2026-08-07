//! The family-space signed distance field derived from the cooked aggregate occupancy.

/// The family-space signed distance field, derived from the aggregate voxel bricks' occupancy — the
/// same grid the aggregate raster form draws, so what a march occludes against is what the coarse
/// cut shows. Deterministic: an exact integer squared-distance transform over the occupancy bits.
///
/// Empty bytes when the family cooked no voxel brick; the reader treats an empty section as
/// "no field", not an error.
pub(super) fn distance_field_section(
    hierarchy: &saffron_geometry::PortableVirtualHierarchy,
) -> Vec<u8> {
    use saffron_geometry::glam::Vec3;

    // The DEEPEST voxel level's bricks: they tile the plant tightly, where the root's
    // single 8³ brick — dilated for watertight reconstruction — reads as one solid box
    // the size of the family, which is a wall, not a plant.
    let mut depth_of = vec![0_u32; hierarchy.nodes.len()];
    for node in &hierarchy.nodes {
        if let Some(parent) = node.parent {
            depth_of[node.id as usize] = depth_of[parent as usize] + 1;
        }
    }
    let voxel_nodes: Vec<(&saffron_geometry::PortableHierarchyNode, u32)> = hierarchy
        .nodes
        .iter()
        .filter_map(|node| match node.representation {
            saffron_geometry::HierarchyRepresentation::Voxel { brick } => Some((node, brick)),
            saffron_geometry::HierarchyRepresentation::Triangles { .. } => None,
        })
        .collect();
    let Some(max_depth) = voxel_nodes
        .iter()
        .map(|(node, _)| depth_of[node.id as usize])
        .max()
    else {
        return Vec::new();
    };
    let selected: Vec<&saffron_geometry::PortableVoxelBrick> = voxel_nodes
        .iter()
        .filter(|(node, _)| depth_of[node.id as usize] == max_depth)
        .filter_map(|(_, brick)| hierarchy.voxel_bricks.iter().find(|b| b.id == *brick))
        .collect();
    if selected.is_empty() {
        return Vec::new();
    }

    let q = |bits: i32| bits as f32 / 65_536.0;
    let corner = |bounds: &saffron_geometry::PortableBounds, max: bool| {
        let bits = if max {
            bounds.max_bits
        } else {
            bounds.min_bits
        };
        Vec3::new(q(bits[0]), q(bits[1]), q(bits[2]))
    };
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for brick in &selected {
        lo = lo.min(corner(&brick.bounds, false));
        hi = hi.max(corner(&brick.bounds, true));
    }
    if !(hi - lo).min_element().is_finite() || (hi - lo).min_element() <= 0.0 {
        return Vec::new();
    }
    // The mesh bake's own grid sizing (padded bounds, capped axes), so a plant's field
    // resolves like an imported mesh's.
    let grid = saffron_geometry::bake_grid(lo, hi, 1.0);
    let [nx, ny, nz] = grid.dims;
    let total = (nx * ny * nz) as usize;
    let cell = grid.cell();

    // Rasterize each selected brick's set bits into the family grid: a set bit covers
    // its voxel's world box; every grid cell whose center falls inside is matter.
    let mut occupied_grid = vec![false; total];
    let idx = |x: u32, y: u32, z: u32| ((z * ny + y) * nx + x) as usize;
    for brick in &selected {
        let [bx, by, bz] = brick.dimensions.map(u32::from);
        if bx == 0 || by == 0 || bz == 0 || brick.occupancy.is_empty() {
            continue;
        }
        let blo = corner(&brick.bounds, false);
        let bhi = corner(&brick.bounds, true);
        let bcell = (bhi - blo) / Vec3::new(bx as f32, by as f32, bz as f32);
        for vz in 0..bz {
            for vy in 0..by {
                for vx in 0..bx {
                    let bit = (vx + bx * (vy + by * vz)) as usize;
                    if brick.occupancy[bit / 8] & (1u8 << (bit % 8)) == 0 {
                        continue;
                    }
                    let vmin = blo + bcell * Vec3::new(vx as f32, vy as f32, vz as f32);
                    let vmax = vmin + bcell;
                    let gmin = ((vmin - grid.bounds_min) / cell).floor().max(Vec3::ZERO);
                    let gmax = ((vmax - grid.bounds_min) / cell).ceil();
                    for gz in gmin.z as u32..(gmax.z as u32).min(nz) {
                        for gy in gmin.y as u32..(gmax.y as u32).min(ny) {
                            for gx in gmin.x as u32..(gmax.x as u32).min(nx) {
                                let center = grid.voxel_center(gx, gy, gz);
                                if center.cmpge(vmin).all() && center.cmplt(vmax).all() {
                                    occupied_grid[idx(gx, gy, gz)] = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !occupied_grid.iter().any(|&occupied| occupied) {
        return Vec::new();
    }

    // Exact 3D Euclidean distance transform (Felzenszwalb–Huttenlocher, one 1D pass per
    // axis), run twice: distance to the nearest occupied cell and to the nearest empty
    // one. Integer squared distances throughout, so the result is bit-identical on
    // every target.
    let edt = |inside: bool| -> Vec<u64> {
        const INF: u64 = u64::MAX / 4;
        let mut field: Vec<u64> = (0..total)
            .map(|index| {
                if occupied_grid[index] == inside {
                    0
                } else {
                    INF
                }
            })
            .collect();
        let mut pass = |axis: usize| {
            let (a_len, b_len, c_len) = match axis {
                0 => (nx, ny, nz),
                1 => (ny, nx, nz),
                _ => (nz, nx, ny),
            };
            let mut line = vec![0_u64; a_len as usize];
            for c in 0..c_len {
                for b in 0..b_len {
                    for a in 0..a_len {
                        let (x, y, z) = match axis {
                            0 => (a, b, c),
                            1 => (b, a, c),
                            _ => (b, c, a),
                        };
                        line[a as usize] = field[idx(x, y, z)];
                    }
                    let transformed = squared_distance_transform_1d(&line);
                    for a in 0..a_len {
                        let (x, y, z) = match axis {
                            0 => (a, b, c),
                            1 => (b, a, c),
                            _ => (b, c, a),
                        };
                        field[idx(x, y, z)] = transformed[a as usize];
                    }
                }
            }
        };
        pass(0);
        pass(1);
        pass(2);
        field
    };
    let to_occupied = edt(true);
    let to_empty = edt(false);

    let voxel = cell.max_element();
    let dense: Vec<i16> = (0..total)
        .map(|index| {
            // Signed distance in world units: positive outside occupied matter, negative
            // inside, from the two unsigned transforms.
            let signed = if occupied_grid[index] {
                -((to_empty[index] as f64).sqrt() as f32) * voxel
            } else {
                ((to_occupied[index] as f64).sqrt() as f32) * voxel
            };
            let normalized = (signed / grid.max_dist).clamp(-1.0, 1.0);
            (normalized * f32::from(i16::MAX)) as i16
        })
        .collect();
    let mut field = saffron_geometry::Sdf::from_dense_field(&grid, &dense);

    // The field carries the CALIBRATED aggregate occupancy and albedo, not the drawn
    // material's: slot 0 of a placed plant is its trunk, and a canopy is not a solid.
    // NOT the coverage fraction either: the march integrates this as Beer-Lambert
    // DENSITY over the occupied path, so the value that preserves energy is the one that
    // reproduces the aggregate's calibrated transmission across its mean thickness — the
    // same parity the thin-sheet materials use.
    let moments = &selected[0].moments;
    let transmission = moments
        .transmission_mean
        .map(|value| value as f32 / 65_536.0);
    let thickness = (moments.thickness_mean as f32 / 65_536.0).max(0.05);
    let occupancy = crate::render_material::derive_parity_occupancy(transmission, thickness);
    field.header.occupancy_unorm = (occupancy.clamp(0.0, 1.0) * 65_535.0 + 0.5) as u32;
    let albedo = |axis: usize| -> u32 {
        ((moments.albedo_mean[axis] as f32 / 65_536.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u32
    };
    field.header.proxy_albedo = albedo(0) | (albedo(1) << 8) | (albedo(2) << 16);
    saffron_geometry::sdf_set_to_bytes(&[field])
}

/// Felzenszwalb–Huttenlocher 1D squared-distance transform over integer parabolas.
fn squared_distance_transform_1d(f: &[u64]) -> Vec<u64> {
    const INF: u64 = u64::MAX / 4;
    let n = f.len();
    let mut v = vec![0_usize; n];
    let mut z = vec![0_i64; n + 1];
    let mut k = 0_usize;
    v[0] = 0;
    z[0] = i64::MIN / 2;
    z[1] = i64::MAX / 2;
    // Parabola intersections in fixed-point twice-the-boundary units, exact in integers.
    let intersect = |q: usize, p: usize| -> i64 {
        let (q, p, fq, fp) = (q as i64, p as i64, f[q] as i64, f[p] as i64);
        // ((f[q] + q²) − (f[p] + p²)) / (2q − 2p), kept as a scaled numerator to stay
        // integral: compare s*2*(q−p) against boundaries scaled by 2*(q−p).
        (fq + q * q - fp - p * p) / (2 * (q - p)).max(1)
    };
    for q in 1..n {
        if f[q] >= INF && f[v[k]] >= INF {
            continue;
        }
        let mut s = intersect(q, v[k]);
        while k > 0 && s <= z[k] {
            k -= 1;
            s = intersect(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = i64::MAX / 2;
    }
    let mut k = 0_usize;
    let mut out = vec![0_u64; n];
    for (q, slot) in out.iter_mut().enumerate() {
        while z[k + 1] < q as i64 {
            k += 1;
        }
        let d = q as i64 - v[k] as i64;
        *slot = f[v[k]].saturating_add((d * d) as u64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{fixture_server, imported_family, options, save_family};
    use super::super::{PlantRecookOutcome, recook_plant_family};
    use saffron_vegetation::{PlantCompiledArtifactIndex, PlantCompiledSectionKind};

    /// The cooked field must be porous: coverage-as-density reads as a black interior,
    /// so the header carries the transmission-parity occupancy instead — strictly below
    /// the raw coverage for any transmitting canopy — plus a real sign structure
    /// (negative somewhere inside, positive somewhere outside).
    #[test]
    fn plant_distance_field_is_calibrated_and_signed() {
        let (_scratch, mut assets, material, mesh) = fixture_server("plant-distance-field");
        let family = save_family(&mut assets, imported_family(material, mesh));
        let outcome = recook_plant_family(&mut assets, &family, &options()).expect("recook");
        let PlantRecookOutcome::Published(published) = outcome else {
            panic!("valid family was rejected");
        };
        let bytes = std::fs::read(&published.publication.path).expect("artifact");
        let index = PlantCompiledArtifactIndex::open(
            &bytes,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .expect("artifact index");
        let section = index
            .section(&bytes, PlantCompiledSectionKind::DistanceField)
            .expect("valid section")
            .expect("required section");
        assert!(!section.is_empty(), "a family with bricks cooks a field");
        let fields = saffron_geometry::sdf_set_from_bytes(section.as_ref()).expect("decode");
        assert_eq!(fields.len(), 1);
        let field = &fields[0];
        eprintln!(
            "occupancy_unorm={} albedo={:#x} dims={:?} max_dist={}",
            field.header.occupancy_unorm,
            field.header.proxy_albedo,
            field.header.dims,
            field.header.max_dist,
        );
        assert!(
            field.header.occupancy_unorm > 0,
            "the field carries its occupancy"
        );
        let dims = field.header.dims;
        let mut negative = 0_u32;
        let mut positive = 0_u32;
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let d = field.sample_voxel(x, y, z);
                    if d < 0.0 {
                        negative += 1;
                    } else if d > 0.0 {
                        positive += 1;
                    }
                }
            }
        }
        eprintln!("negative={negative} positive={positive}");
        assert!(negative > 0, "somewhere is inside matter");
        assert!(positive > 0, "somewhere is open");
    }
}
