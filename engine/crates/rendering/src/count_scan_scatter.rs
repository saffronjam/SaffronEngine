//! Capacity-proven count/scan/scatter planning for GPU-generated work.

/// A capacity overflow returned instead of truncating generated work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountScanScatterOverflow {
    /// Exact number of output records requested by the count stage.
    pub required: u64,
    /// Number of records available in the destination allocation.
    pub capacity: u64,
    /// Coarser resident records that remain drawable while the allocation grows.
    pub drawable_fallbacks: u64,
}

/// Result of a count/scan capacity proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CountScanScatterOutcome {
    /// Every counted record has a unique in-bounds destination.
    Ready(CountScanScatterPlan),
    /// No scatter may run; the caller draws the reported resident fallback set.
    Overflow(CountScanScatterOverflow),
}

/// Typed construction and indexing failures for count/scan/scatter work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CountScanScatterError {
    /// The exclusive-offset table could not reserve its exact length.
    #[error("cannot allocate count/scan offsets for {bucket_count} buckets")]
    AllocationFailed {
        /// Requested number of offset entries.
        bucket_count: usize,
    },
    /// The count total is not representable.
    #[error("count/scan total overflowed at bucket {bucket}")]
    CountOverflow {
        /// Bucket whose count overflowed the running total.
        bucket: usize,
    },
    /// A visible-list overflow did not provide any resident fallback records.
    #[error("count/scan overflow has no drawable resident fallback")]
    MissingDrawableFallback,
    /// A scatter lookup addressed a bucket outside the count table.
    #[error("scatter bucket {bucket} is outside {bucket_count} buckets")]
    BucketOutOfBounds {
        /// Requested bucket.
        bucket: usize,
        /// Number of counted buckets.
        bucket_count: usize,
    },
    /// A scatter lookup addressed a record outside its bucket's exact count.
    #[error("scatter local index {local_index} is outside bucket {bucket}'s count {count}")]
    LocalIndexOutOfBounds {
        /// Requested bucket.
        bucket: usize,
        /// Requested local record.
        local_index: u64,
        /// Exact count declared for the bucket.
        count: u64,
    },
}

/// Exact exclusive offsets and total produced by the scan stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountScanScatterPlan {
    counts: Vec<u64>,
    offsets: Vec<u64>,
    total: u64,
}

impl CountScanScatterPlan {
    /// Counts and exclusively scans buckets, proving the result against `capacity`.
    ///
    /// When the exact total exceeds capacity, no clamped plan is returned. `drawable_fallbacks`
    /// must name the coarser resident records that stay drawable under that pressure.
    pub fn build(
        counts: impl IntoIterator<Item = u64>,
        capacity: u64,
        drawable_fallbacks: u64,
    ) -> Result<CountScanScatterOutcome, CountScanScatterError> {
        let counts = counts.into_iter().collect::<Vec<_>>();
        let mut offsets = Vec::new();
        offsets.try_reserve_exact(counts.len()).map_err(|_| {
            CountScanScatterError::AllocationFailed {
                bucket_count: counts.len(),
            }
        })?;
        let mut total = 0_u64;
        for (bucket, count) in counts.iter().copied().enumerate() {
            offsets.push(total);
            total = total
                .checked_add(count)
                .ok_or(CountScanScatterError::CountOverflow { bucket })?;
        }
        if total > capacity {
            if drawable_fallbacks == 0 {
                return Err(CountScanScatterError::MissingDrawableFallback);
            }
            return Ok(CountScanScatterOutcome::Overflow(
                CountScanScatterOverflow {
                    required: total,
                    capacity,
                    drawable_fallbacks,
                },
            ));
        }
        Ok(CountScanScatterOutcome::Ready(Self {
            counts,
            offsets,
            total,
        }))
    }

    /// Exact count for each bucket.
    pub fn counts(&self) -> &[u64] {
        &self.counts
    }

    /// Exclusive output offsets for each bucket.
    pub fn offsets(&self) -> &[u64] {
        &self.offsets
    }

    /// Total output records proven to fit the destination.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Returns the unique output offset for one bucket-local record.
    pub fn scatter_offset(
        &self,
        bucket: usize,
        local_index: u64,
    ) -> Result<u64, CountScanScatterError> {
        let Some(&count) = self.counts.get(bucket) else {
            return Err(CountScanScatterError::BucketOutOfBounds {
                bucket,
                bucket_count: self.counts.len(),
            });
        };
        if local_index >= count {
            return Err(CountScanScatterError::LocalIndexOutOfBounds {
                bucket,
                local_index,
                count,
            });
        }
        Ok(self.offsets[bucket] + local_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_scan_proves_every_scatter_offset() {
        let CountScanScatterOutcome::Ready(plan) =
            CountScanScatterPlan::build([3, 0, 2], 5, 0).unwrap()
        else {
            panic!("exact capacity must fit");
        };
        assert_eq!(plan.offsets(), [0, 3, 3]);
        assert_eq!(plan.total(), 5);
        assert_eq!(plan.scatter_offset(0, 2), Ok(2));
        assert_eq!(plan.scatter_offset(2, 1), Ok(4));
        assert_eq!(
            plan.scatter_offset(1, 0),
            Err(CountScanScatterError::LocalIndexOutOfBounds {
                bucket: 1,
                local_index: 0,
                count: 0,
            })
        );
    }

    #[test]
    fn capacity_pressure_reports_exact_total_and_fallback() {
        assert_eq!(
            CountScanScatterPlan::build([4, 5], 8, 2),
            Ok(CountScanScatterOutcome::Overflow(
                CountScanScatterOverflow {
                    required: 9,
                    capacity: 8,
                    drawable_fallbacks: 2,
                }
            ))
        );
        assert_eq!(
            CountScanScatterPlan::build([9], 8, 0),
            Err(CountScanScatterError::MissingDrawableFallback)
        );
    }

    #[test]
    fn arithmetic_overflow_is_typed() {
        assert_eq!(
            CountScanScatterPlan::build([u64::MAX, 1], u64::MAX, 1),
            Err(CountScanScatterError::CountOverflow { bucket: 1 })
        );
    }
}
