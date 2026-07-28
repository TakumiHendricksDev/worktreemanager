//! The real clock.
//!
//! Lives in this crate because it is an OS adapter like the others, and because
//! `clippy.toml` bans `SystemTime::now`/`Instant::now` everywhere else — so this is
//! the one place they are legitimate, and it carries the corresponding `#[allow]`.
//! Everything upstream takes a [`Clock`], which is what makes cache-expiry and
//! `now.*` token behaviour testable at a fixed instant.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use wtm_core::ports::clock::Clock;

/// The one sanctioned `Instant::now()` in the workspace.
///
/// `clippy.toml` bans `Instant::now` so that *use-cases* take a [`Clock`] and stay
/// deterministic. Adapters in this crate genuinely need a monotonic reading for
/// their own deadlines, and threading a `Clock` into a poll loop would buy nothing.
/// Funnelling them through one function keeps that exception to a single reviewable
/// place instead of an `#[allow]` at every call site.
#[must_use]
pub(crate) fn instant_now() -> Instant {
    #[allow(clippy::disallowed_methods)]
    Instant::now()
}

/// Wall-clock and monotonic time from the OS.
#[derive(Debug, Clone)]
pub struct SystemClock {
    /// Fixed reference point for the monotonic counter, so it starts near zero and
    /// cannot overflow a `u64` of milliseconds in any realistic session.
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: instant_now(),
        }
    }

    /// Local time, falling back to UTC.
    ///
    /// `time` cannot always determine the local offset from a multi-threaded
    /// process — it declines rather than risk an unsound `localtime_r` call. A
    /// timestamp in UTC is a small cosmetic wrong; refusing to render a date at all
    /// would be a functional one.
    fn now_local_or_utc() -> OffsetDateTime {
        #[allow(clippy::disallowed_methods)]
        let now = SystemTime::now();
        let utc = OffsetDateTime::from(now);
        OffsetDateTime::now_local().unwrap_or(utc)
    }
}

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        #[allow(clippy::disallowed_methods)]
        let now = SystemTime::now();
        now.duration_since(UNIX_EPOCH)
            // Before 1970 means the system clock is badly wrong; 0 is a more useful
            // answer than a panic.
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    fn today(&self) -> String {
        let now = Self::now_local_or_utc();
        format!(
            "{:04}-{:02}-{:02}",
            now.year(),
            u8::from(now.month()),
            now.day()
        )
    }

    fn now_iso(&self) -> String {
        Self::now_local_or_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| self.today())
    }

    fn monotonic_ms(&self) -> u64 {
        u64::try_from(instant_now().duration_since(self.origin).as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_time_is_plausible() {
        // Later than 2024-01-01, earlier than 2100.
        let ms = SystemClock::new().now_unix_ms();
        assert!(ms > 1_704_067_200_000, "suspiciously early: {ms}");
        assert!(ms < 4_102_444_800_000, "suspiciously late: {ms}");
    }

    #[test]
    fn today_is_an_iso_date() {
        let today = SystemClock::new().today();
        assert_eq!(today.len(), 10, "got {today}");
        let parts: Vec<&str> = today.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!(
            parts[0]
                .parse::<u16>()
                .is_ok_and(|y| (2000..2100).contains(&y))
        );
        assert!(parts[1].parse::<u8>().is_ok_and(|m| (1..=12).contains(&m)));
        assert!(parts[2].parse::<u8>().is_ok_and(|d| (1..=31).contains(&d)));
    }

    #[test]
    fn iso_timestamp_round_trips() {
        let iso = SystemClock::new().now_iso();
        assert!(
            OffsetDateTime::parse(&iso, &Rfc3339).is_ok(),
            "not valid RFC 3339: {iso}"
        );
    }

    #[test]
    fn monotonic_never_goes_backwards() {
        let clock = SystemClock::new();
        let first = clock.monotonic_ms();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = clock.monotonic_ms();
        assert!(second >= first, "{second} < {first}");
        assert!(
            second - first >= 15,
            "expected ~20ms, got {}",
            second - first
        );
    }
}
