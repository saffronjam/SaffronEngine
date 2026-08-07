//! Time primitives.

/// A span of time, in seconds.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Default)]
pub struct TimeSpan {
    /// The duration in seconds.
    pub seconds: f32,
}

impl TimeSpan {
    #[must_use]
    pub const fn from_seconds(seconds: f32) -> Self {
        Self { seconds }
    }

    /// The span expressed in milliseconds.
    #[must_use]
    pub const fn to_milliseconds(self) -> f32 {
        self.seconds * 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milliseconds_is_seconds_times_thousand() {
        assert_eq!(TimeSpan::from_seconds(2.5).to_milliseconds(), 2500.0);
        assert_eq!(TimeSpan::default().to_milliseconds(), 0.0);
        assert_eq!(TimeSpan::from_seconds(0.001).to_milliseconds(), 1.0);
    }
}
