use std::fmt::Debug;

use tokio::time::Instant;

#[cfg(test)]
use super::relative_time;

/// Store the current active config and next config.
#[derive(Debug, Default)]
pub struct CurrentConfig<C> {
    last: Option<C>,
    current: Option<(Instant, C)>,
}

impl<C: Debug> CurrentConfig<C> {
    pub fn reset(&mut self) {
        self.current = None;
        self.last = None;
    }

    pub fn update(&mut self, current_value: C, valid_since: Instant) {
        #[cfg(test)]
        tracing::debug!(
            "update called at logical {:?} wallclock {:?}",
            relative_time(valid_since),
            relative_time(Instant::now())
        );
        self.last = self.current.take().map(|(_, value)| value);
        self.current = (valid_since, current_value).into();
    }

    pub fn update_get_last(&mut self, current_value: C, valid_since: Instant) -> Option<&C> {
        self.update(current_value, valid_since);
        self.last.as_ref()
    }

    /// Get the config value for a given timestamp.
    /// It is expected that the timestamp passed into this function is non-decreasing.
    ///
    /// According to the given timestamp,
    ///
    /// 0)  If `current` value is not set.
    ///     a) Try to use the `last`
    ///
    /// 1)  If `current` value has not yet taken effect,
    ///     a) Try to use the `last`
    ///     b) Use the `current` value
    ///
    /// 2)  If `current` value is in its valid period,
    ///     Use the current value
    ///
    /// As long as `self.update` has been called at least once since the last `self.reset`,
    /// it is impossible for `self.current` and `self.last` to be `None`.
    /// So if this function ever returns `None`, the caller should unwrap it with
    /// some default value.
    pub fn get_current(&self, timestamp: Instant) -> Option<&C> {
        #[cfg(test)]
        tracing::debug!(
            "get_current called at logical {:?} wallclock {:?}",
            relative_time(timestamp),
            relative_time(Instant::now())
        );

        if let Some((valid_since, value)) = self.current.as_ref() {
            if timestamp >= *valid_since {
                return value.into();
            }
            return self.last.as_ref().unwrap_or(value).into();
        }
        self.last.as_ref()
    }
}
