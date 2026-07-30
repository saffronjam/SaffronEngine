//! The per-generation bounds hierarchy over a cell's macro rows.

use glam::DVec3;
use saffron_spatial::WorldBounds;

use crate::{Error, Result};

#[derive(Clone, Debug, Default)]
pub(super) struct MacroBvh {
    nodes: Vec<MacroBvhNode>,
    row_bounds: Vec<WorldBounds>,
    root: Option<u32>,
}

#[derive(Clone, Debug)]
enum MacroBvhNode {
    Leaf {
        bounds: WorldBounds,
        rows: Vec<u32>,
    },
    Branch {
        bounds: WorldBounds,
        left: u32,
        right: u32,
    },
}

impl MacroBvhNode {
    const fn bounds(&self) -> WorldBounds {
        match self {
            Self::Leaf { bounds, .. } | Self::Branch { bounds, .. } => *bounds,
        }
    }
}

impl MacroBvh {
    pub(super) fn build(bounds: &[WorldBounds]) -> Result<Self> {
        if bounds.is_empty() {
            return Ok(Self::default());
        }
        let mut value = Self {
            nodes: Vec::new(),
            row_bounds: bounds.to_vec(),
            root: None,
        };
        let rows = (0..bounds.len())
            .map(|row| u32::try_from(row).map_err(|_| Error::NumericOverflow))
            .collect::<Result<Vec<_>>>()?;
        value.root = Some(value.build_node(bounds, rows)?);
        Ok(value)
    }

    fn build_node(&mut self, source: &[WorldBounds], mut rows: Vec<u32>) -> Result<u32> {
        let bounds = rows
            .iter()
            .map(|row| source[*row as usize])
            .reduce(WorldBounds::union)
            .ok_or(Error::NumericOverflow)?;
        if rows.len() <= 8 {
            rows.sort_unstable();
            return self.push(MacroBvhNode::Leaf { bounds, rows });
        }
        let min = bounds.min_ticks();
        let max = bounds.max_ticks_exclusive();
        let axis = (0..3)
            .max_by_key(|axis| max[*axis] - min[*axis])
            .unwrap_or(0);
        rows.sort_unstable_by_key(|row| {
            let row_bounds = source[*row as usize];
            row_bounds.min_ticks()[axis] + row_bounds.max_ticks_exclusive()[axis]
        });
        let right = rows.split_off(rows.len() / 2);
        let left = self.build_node(source, rows)?;
        let right = self.build_node(source, right)?;
        self.push(MacroBvhNode::Branch {
            bounds,
            left,
            right,
        })
    }

    fn push(&mut self, node: MacroBvhNode) -> Result<u32> {
        let index = u32::try_from(self.nodes.len()).map_err(|_| Error::NumericOverflow)?;
        self.nodes.push(node);
        Ok(index)
    }

    pub(super) fn query_bounds(&self, bounds: WorldBounds) -> Vec<u32> {
        let mut result = Vec::new();
        let Some(root) = self.root else {
            return result;
        };
        let mut pending = vec![root];
        while let Some(index) = pending.pop() {
            match &self.nodes[index as usize] {
                MacroBvhNode::Leaf {
                    bounds: node_bounds,
                    rows,
                } => {
                    if bounds_intersect(*node_bounds, bounds) {
                        result.extend(rows.iter().copied());
                    }
                }
                MacroBvhNode::Branch {
                    bounds: node_bounds,
                    left,
                    right,
                } => {
                    if bounds_intersect(*node_bounds, bounds) {
                        pending.push(*right);
                        pending.push(*left);
                    }
                }
            }
        }
        result.retain(|row| bounds_intersect(self.row_bounds[*row as usize], bounds));
        result.sort_unstable();
        result
    }

    pub(super) fn rows_by_nearness(&self, point: DVec3) -> Vec<u32> {
        let mut rows = self.query_all();
        rows.sort_by(|left, right| {
            let left_distance = self.row_bounds(*left).map_or(f64::INFINITY, |bounds| {
                distance_squared_to_bounds(point, bounds)
            });
            let right_distance = self.row_bounds(*right).map_or(f64::INFINITY, |bounds| {
                distance_squared_to_bounds(point, bounds)
            });
            left_distance
                .total_cmp(&right_distance)
                .then_with(|| left.cmp(right))
        });
        rows
    }

    pub(super) fn query_ray(
        &self,
        origin: DVec3,
        direction: DVec3,
        maximum: f64,
    ) -> Vec<(u32, f64)> {
        let mut result = Vec::new();
        let Some(root) = self.root else {
            return result;
        };
        let mut pending = vec![root];
        while let Some(index) = pending.pop() {
            let node = &self.nodes[index as usize];
            if ray_bounds_distance(origin, direction, node.bounds(), maximum).is_none() {
                continue;
            }
            match node {
                MacroBvhNode::Leaf { rows, .. } => {
                    result.extend(rows.iter().filter_map(|row| {
                        self.row_bounds(*row)
                            .and_then(|bounds| {
                                ray_bounds_distance(origin, direction, bounds, maximum)
                            })
                            .map(|distance| (*row, distance))
                    }));
                }
                MacroBvhNode::Branch { left, right, .. } => {
                    pending.push(*right);
                    pending.push(*left);
                }
            }
        }
        result.sort_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        result
    }

    fn query_all(&self) -> Vec<u32> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let mut pending = vec![root];
        let mut rows = Vec::new();
        while let Some(index) = pending.pop() {
            match &self.nodes[index as usize] {
                MacroBvhNode::Leaf { rows: leaf, .. } => rows.extend(leaf),
                MacroBvhNode::Branch { left, right, .. } => {
                    pending.push(*right);
                    pending.push(*left);
                }
            }
        }
        rows
    }

    fn row_bounds(&self, row: u32) -> Option<WorldBounds> {
        self.row_bounds.get(row as usize).copied()
    }
}

fn bounds_intersect(left: WorldBounds, right: WorldBounds) -> bool {
    let left_min = left.min_ticks();
    let left_max = left.max_ticks_exclusive();
    let right_min = right.min_ticks();
    let right_max = right.max_ticks_exclusive();
    (0..3).all(|axis| left_min[axis] < right_max[axis] && right_min[axis] < left_max[axis])
}

pub(super) fn distance_squared_to_bounds(point: DVec3, bounds: WorldBounds) -> f64 {
    let minimum = ticks_to_meters(bounds.min_ticks());
    let maximum = ticks_to_meters(bounds.max_ticks_exclusive());
    (0..3)
        .map(|axis| {
            let delta = if point[axis] < minimum[axis] {
                minimum[axis] - point[axis]
            } else if point[axis] > maximum[axis] {
                point[axis] - maximum[axis]
            } else {
                0.0
            };
            delta * delta
        })
        .sum()
}

fn ray_bounds_distance(
    origin: DVec3,
    direction: DVec3,
    bounds: WorldBounds,
    maximum: f64,
) -> Option<f64> {
    let minimum = ticks_to_meters(bounds.min_ticks());
    let maximum_bounds = ticks_to_meters(bounds.max_ticks_exclusive());
    let mut enter = 0.0_f64;
    let mut exit = maximum;
    for axis in 0..3 {
        if direction[axis] == 0.0 {
            if origin[axis] < minimum[axis] || origin[axis] > maximum_bounds[axis] {
                return None;
            }
            continue;
        }
        let inverse = direction[axis].recip();
        let first = (minimum[axis] - origin[axis]) * inverse;
        let second = (maximum_bounds[axis] - origin[axis]) * inverse;
        enter = enter.max(first.min(second));
        exit = exit.min(first.max(second));
        if exit < enter {
            return None;
        }
    }
    (enter <= maximum).then_some(enter)
}

fn ticks_to_meters(ticks: [i128; 3]) -> DVec3 {
    DVec3::new(ticks[0] as f64, ticks[1] as f64, ticks[2] as f64)
        / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER)
}
