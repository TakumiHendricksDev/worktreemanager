/**
 * What each provider account has left, mirrored from Rust's usage registry.
 *
 * # Rust's record, not this window's
 *
 * The figures belong to accounts, and every pane and window shows the same ones. Rust files each
 * report a session makes and each answer to asking a provider, and announces the provider's whole
 * record as `usage:limits`. This store replaces its copy with that record; it never merges, so two
 * windows cannot disagree. See `src-tauri/src/usage.rs`.
 *
 * # Asked when looked at, never on a timer
 *
 * `refresh` starts a short-lived CLI per provider. The usage dialog calls it when it opens and
 * when Refresh is pressed, and a pane's context card calls `ensure` once per provider per run.
 * Polling is banned (ARCHITECTURE §8), and a Codex app server started every few minutes to keep a
 * bar fresh would be a poor trade for a figure a running session already reports.
 *
 * # Times are absolute
 *
 * "Resets 3:00 PM", not "resets in 4 hr 11 min". A countdown is wrong a minute after it is drawn
 * unless something redraws it, and the only thing that could is a timer.
 */

import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import type { AccountUsage, LimitWindow } from '../ipc/types';

/** At or over this share, a window's meter turns amber. The percentage beside it says why. */
const WARN_PERCENT = 80;

class Usage {
  /** Rust's records, keyed by provider id. */
  accounts = $state<Record<string, AccountUsage>>({});

  /** Providers being asked right now. */
  asking = $state<Record<string, boolean>>({});

  /**
   * Seed from Rust and follow its announcements. Returns the unlisten.
   *
   * Listening before seeding, so an announcement that lands while the seed is in flight is not
   * then overwritten by the older seed. The seed only fills providers the listener has not.
   */
  async init(): Promise<() => void> {
    const off = await listen<AccountUsage>('usage:limits', (event) => {
      this.accounts[event.payload.provider] = event.payload;
    });
    const seed = await commands.usageLimits().catch(() => []);
    for (const account of seed) {
      this.accounts[account.provider] ??= account;
    }
    return off;
  }

  /** Ask one provider for what it will say on demand. Joins an ask already in flight. */
  async refresh(provider: string): Promise<void> {
    if (this.asking[provider]) return;
    this.asking[provider] = true;
    try {
      this.accounts[provider] = await commands.refreshUsageLimits(provider);
    } catch {
      // Only an unknown provider id is an error reply; a failed ask comes back as a record with
      // `error` set. Nothing to add to that here.
    } finally {
      this.asking[provider] = false;
    }
  }

  /** Ask once per run, for a view that wants something to show but has no Refresh of its own. */
  ensure(provider: string): void {
    if (this.accounts[provider]?.askedAt == null) void this.refresh(provider);
  }
}

export const usage = new Usage();

/**
 * Whether a window's reset has already passed, so its figure describes a window that is over.
 *
 * Such a window is shown without a percentage rather than at its old one: after a reset the true
 * figure is lower, by an amount nobody has reported yet.
 */
export function elapsed(window: LimitWindow, now: number): boolean {
  return window.resetsAt !== null && window.resetsAt * 1000 <= now;
}

/** "5-hour", "weekly", "3-day": a window named from its length. */
export function windowLength(window: LimitWindow): string | null {
  const minutes = window.minutes;
  if (minutes === null || minutes <= 0) return null;
  if (minutes === 10_080) return 'weekly';
  if (minutes % 1440 === 0) return `${minutes / 1440}-day`;
  if (minutes % 60 === 0) return `${minutes / 60}-hour`;
  return `${minutes}-minute`;
}

/** "5-hour limit", "Weekly limit · Opus": the row's name. */
export function limitLabel(window: LimitWindow): string {
  const length = windowLength(window);
  const name = length
    ? `${length.charAt(0).toUpperCase()}${length.slice(1)} limit`
    : 'Limit';
  return window.scope ? `${name} · ${window.scope}` : name;
}

/** "Resets 3:00 PM" today, "Resets Sat 3:00 PM" this week, "Reset at 3:00 PM" once it has. */
export function resetText(window: LimitWindow, now: number): string {
  if (window.resetsAt === null) return '';
  const at = new Date(window.resetsAt * 1000);
  const time = new Intl.DateTimeFormat(undefined, { timeStyle: 'short' }).format(at);
  if (elapsed(window, now)) return `Reset at ${time}`;
  const today = new Date(now);
  const sameDay = at.toDateString() === today.toDateString();
  if (sameDay) return `Resets ${time}`;
  const withinWeek = at.getTime() - now < 7 * 24 * 60 * 60 * 1000;
  const day = new Intl.DateTimeFormat(
    undefined,
    withinWeek ? { weekday: 'short' } : { month: 'short', day: 'numeric' },
  ).format(at);
  return `Resets ${day} ${time}`;
}

/** "Updated 3:12 PM", or with the day when it was not today. */
export function updatedText(at: number, now: number): string {
  const when = new Date(at);
  const sameDay = when.toDateString() === new Date(now).toDateString();
  const format = new Intl.DateTimeFormat(
    undefined,
    sameDay
      ? { timeStyle: 'short' }
      : { weekday: 'short', hour: 'numeric', minute: '2-digit' },
  );
  return `Updated ${format.format(when)}`;
}

/** The meter's tone for a share spent. Paired with the percentage, never shown alone. */
export function toneFor(percent: number): 'accent' | 'warn' | 'danger' {
  if (percent >= 100) return 'danger';
  if (percent >= WARN_PERCENT) return 'warn';
  return 'accent';
}

/**
 * The window with the least room left that has not already reset, for a one-line summary.
 *
 * `null` when there is no live figure, so a caller says nothing rather than quoting an old one.
 */
export function tightest(
  account: AccountUsage | undefined,
  now: number,
): LimitWindow | null {
  let worst: LimitWindow | null = null;
  for (const window of account?.limits.windows ?? []) {
    if (elapsed(window, now)) continue;
    if (worst === null || window.usedPercent > worst.usedPercent) worst = window;
  }
  return worst;
}

/** "40% of weekly limit used", "5-hour limit reached": one window in a phrase, for a menu row. */
export function summaryText(window: LimitWindow): string {
  const length = windowLength(window);
  const name = [length, window.scope, 'limit'].filter(Boolean).join(' ');
  if (window.usedPercent >= 100)
    return `${name.charAt(0).toUpperCase()}${name.slice(1)} reached`;
  return `${Math.round(window.usedPercent)}% of ${name} used`;
}

/**
 * What to say for a provider with no figures, by provider.
 *
 * Provider-specific because the reasons are. Claude answers when asked, so having nothing means
 * an account with no plan limits (an API key) or an ask that failed; a reply in any pane still
 * brings the five-hour and weekly figures. Cursor has none it will share. Anything else gets the
 * plain fact.
 */
export function noFiguresText(provider: string, label: string): string {
  if (provider === 'claude')
    return `${label} didn't report any limits when asked. An account on an API key has none; otherwise they also arrive with its next reply in any pane.`;
  if (provider === 'cursor')
    return `${label}'s CLI shares its plan but not its usage. The figures are on its own dashboard.`;
  return `${label} has not reported any limits yet.`;
}
