//! How much of a provider account's allowance is spent, as the provider last said.
//!
//! # Why this is not [`Usage`](super::Usage)
//!
//! `Usage` belongs to a turn: tokens in, tokens out, how full one session's context is. These
//! figures belong to the *account*. Every pane on the same provider shares them, and so does the
//! provider's own CLI running in a terminal somewhere else. Folding them into `Usage` would make
//! each pane hold its own stale copy of a number none of them owns.
//!
//! # Why a window carries its length rather than a name
//!
//! Claude calls its windows `five_hour` and `seven_day`, and Codex calls them `primary` and
//! `secondary`. Codex's `primary` is the *weekly* window on a plan without a five-hour one, so its
//! names say nothing about what the window is. Both sides do state a length, either outright or
//! in the name, so a window is identified by its minutes and the UI names it from those.
//!
//! # Why reports are merged rather than replaced
//!
//! The reports arrive by different routes and each one carries only part of the picture. Claude's
//! windows come with every turn and its plan only from `claude auth status`. Codex's rolling
//! update is documented as sparse: "clients should merge available values into the most recent
//! read". [`UsageLimits::absorb`] is that merge, and it lives here so the rules can be tested
//! without a process.

use serde::{Deserialize, Serialize};

/// One rolling allowance on an account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitWindow {
    /// How long the window rolls over, in minutes: 300 for five hours, 10 080 for a week.
    ///
    /// `None` when the provider did not say. A window with no length is still shown, but nothing
    /// can be called "weekly" on a guess.
    pub minutes: Option<u64>,
    /// What the window is limited to, when that is narrower than the whole account, such as one
    /// model's name.
    pub scope: Option<String>,
    /// How much is spent, from 0 to 100.
    ///
    /// A percentage on the wire whatever the provider sends. Claude sends a fraction, Codex a
    /// percentage. Converting at the edge means the UI never has to ask which one it got.
    pub used_percent: f64,
    /// Unix seconds at which the window resets, when the provider says.
    pub resets_at: Option<u64>,
}

impl LimitWindow {
    /// The identity two reports of the same window share.
    fn key(&self) -> (Option<u64>, Option<&str>) {
        (self.minutes, self.scope.as_deref())
    }
}

/// What one provider's account has left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimits {
    /// The catalogue id of the provider these figures belong to.
    ///
    /// Carried in the value rather than inferred from the session that reported it, because the
    /// app keeps one record per provider and the record should be able to say whose it is.
    pub provider: String,
    /// The plan as a reader would name it, such as "Team" or "Pro".
    pub plan: Option<String>,
    /// Shortest first, with account-wide windows ahead of narrower ones.
    pub windows: Vec<LimitWindow>,
    /// Free full resets the provider has granted and not yet used. Only Codex has these.
    pub reset_credits: Option<u32>,
}

impl UsageLimits {
    /// A report that says nothing yet, for a provider whose first figures are still to come.
    #[must_use]
    pub fn empty(provider: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            plan: None,
            windows: Vec::new(),
            reset_credits: None,
        }
    }

    /// Fold a later report into this one.
    ///
    /// `complete` says the later report lists every window the account has, which is true of a
    /// Codex `account/rateLimits/read` reply and of nothing else. A complete report replaces the
    /// windows outright, so one that the account no longer has (after a plan change, say) goes
    /// away. A partial one replaces only the windows it names. The plan and the reset count are
    /// kept when the later report is silent about them, because silence in a partial report means
    /// "not included" and not "gone".
    pub fn absorb(&mut self, later: Self, complete: bool) {
        if later.plan.is_some() {
            self.plan = later.plan;
        }
        if later.reset_credits.is_some() {
            self.reset_credits = later.reset_credits;
        }
        if complete {
            self.windows = later.windows;
        } else {
            for window in later.windows {
                match self.windows.iter_mut().find(|w| w.key() == window.key()) {
                    Some(existing) => *existing = window,
                    None => self.windows.push(window),
                }
            }
        }
        // Account-wide windows first, then by length. A window with no stated length sorts last
        // among its group, after every window that has one.
        self.windows.sort_by_key(|w| {
            (
                w.scope.is_some(),
                w.minutes.unwrap_or(u64::MAX),
                w.scope.clone(),
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(minutes: u64, used: f64, resets_at: u64) -> LimitWindow {
        LimitWindow {
            minutes: Some(minutes),
            scope: None,
            used_percent: used,
            resets_at: Some(resets_at),
        }
    }

    fn report(windows: Vec<LimitWindow>) -> UsageLimits {
        UsageLimits {
            windows,
            ..UsageLimits::empty("claude")
        }
    }

    #[test]
    fn a_partial_report_replaces_the_windows_it_names_and_keeps_the_rest() {
        let mut held = report(vec![window(300, 20.0, 100), window(10_080, 40.0, 900)]);
        held.absorb(report(vec![window(300, 35.0, 100)]), false);

        assert_eq!(
            held.windows,
            vec![window(300, 35.0, 100), window(10_080, 40.0, 900)]
        );
    }

    #[test]
    fn a_complete_report_drops_a_window_the_account_no_longer_has() {
        let mut held = report(vec![window(300, 20.0, 100), window(10_080, 40.0, 900)]);
        held.absorb(report(vec![window(10_080, 41.0, 900)]), true);

        assert_eq!(held.windows, vec![window(10_080, 41.0, 900)]);
    }

    #[test]
    fn a_report_without_windows_leaves_the_windows_alone_and_still_sets_the_plan() {
        // Claude's plan comes from `claude auth status`, which knows nothing about the windows its
        // turns report. Treating its empty list as "no windows" would blank the meters every time
        // the plan was asked for.
        let mut held = report(vec![window(300, 20.0, 100)]);
        held.absorb(
            UsageLimits {
                plan: Some("Team".to_owned()),
                ..UsageLimits::empty("claude")
            },
            false,
        );

        assert_eq!(held.plan.as_deref(), Some("Team"));
        assert_eq!(held.windows, vec![window(300, 20.0, 100)]);
    }

    #[test]
    fn a_later_report_that_is_silent_about_the_plan_does_not_erase_it() {
        let mut held = UsageLimits {
            plan: Some("Pro".to_owned()),
            reset_credits: Some(3),
            ..UsageLimits::empty("codex")
        };
        held.absorb(report(vec![window(10_080, 6.0, 900)]), false);

        assert_eq!(held.plan.as_deref(), Some("Pro"));
        assert_eq!(held.reset_credits, Some(3));
    }

    #[test]
    fn windows_are_kept_shortest_first_with_narrower_scopes_after_the_account_wide_ones() {
        let mut held = UsageLimits::empty("claude");
        let scoped = LimitWindow {
            scope: Some("Opus".to_owned()),
            ..window(10_080, 22.0, 900)
        };
        held.absorb(
            report(vec![
                scoped.clone(),
                window(10_080, 40.0, 900),
                window(300, 20.0, 100),
            ]),
            false,
        );

        assert_eq!(
            held.windows,
            vec![window(300, 20.0, 100), window(10_080, 40.0, 900), scoped]
        );
    }
}
