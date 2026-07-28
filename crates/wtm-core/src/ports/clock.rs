//! Time.
//!
//! A one-method trait looks like ceremony until you try to test a cache TTL or a
//! `now.date` token. `SystemTime::now` is banned repo-wide by `clippy.toml`'s
//! `disallowed-methods` precisely so time enters through here and use-cases stay
//! deterministic.

/// Wall-clock and monotonic time.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch. Used for cache expiry and for the
    /// `now.unix` token.
    fn now_unix_ms(&self) -> u64;

    /// `YYYY-MM-DD` in local time, for the `now.date` token.
    fn today(&self) -> String;

    /// RFC 3339 timestamp, for the `now.iso` token.
    fn now_iso(&self) -> String;

    /// A monotonically increasing millisecond counter for measuring durations.
    ///
    /// Separate from [`Self::now_unix_ms`] because the wall clock can jump
    /// backwards, and a command that appears to take negative time is worse than
    /// no measurement.
    fn monotonic_ms(&self) -> u64;
}
