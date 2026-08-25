/**
 * Dictation: the microphone button's settings and its one piece of live state.
 *
 * # Off until asked for, twice
 *
 * This is the only feature in wtm that sends anything off the machine, so it does not turn itself
 * on. `ui.dictate` starts absent, which means "never asked" rather than "off" — the same tri-state
 * `attention` uses for notifications, and for the same reason: a user who has declined and a user
 * who has not been asked want different things from the UI.
 *
 * Enabling it is gated on a consent step that names the destination, because "wtm records your
 * microphone and uploads it" is not something anybody should discover from a network log. The
 * backend re-checks the preference before opening the microphone, so this store is the UI's copy of
 * that decision rather than the thing enforcing it — see `src-tauri/src/dictate.rs`.
 *
 * # Why the settings are strings
 *
 * `set_pref` is stringly typed by design, and `user.rs` documents unknown keys landing in
 * `ui.extra` so a new preference needs no Rust change. Keyterms pay for that with a comma-separated
 * list, which is lossy against a term containing a comma — a real limitation, written down here
 * rather than discovered, and cheap next to a schema migration for a feature this size.
 */

import { commands } from '../ipc/commands';
import { errorMessage, type DictationStatus } from '../ipc/types';

/** Whether the microphone has been turned on, or never asked about. */
export type DictateEnabled = 'ask' | 'on' | 'off';

/**
 * How the button behaves.
 *
 * `hold` matches Claude Code's own default and is the safer one: the recording cannot outlive the
 * press, so there is no state to forget about. `tap` exists because holding a mouse button for a
 * paragraph is genuinely worse than pressing it twice.
 */
export type DictateMode = 'hold' | 'tap';

export const DEFAULT_MODE: DictateMode = 'hold';
export const DEFAULT_LANGUAGE = 'en';
export const DEFAULT_MAX_SECONDS = 120;

/** The host the audio goes to. Shown in the consent step, so it is not a thing to guess at. */
export const DESTINATION = 'api.deepgram.com';

const ENABLED = 'ui.dictate';
const MODE = 'ui.dictate_mode';
const LANGUAGE = 'ui.dictate_language';
const KEYTERMS = 'ui.dictate_keyterms';
const MAX_SECONDS = 'ui.dictate_max_seconds';

class Dictation {
  enabled = $state<DictateEnabled>('ask');
  mode = $state<DictateMode>(DEFAULT_MODE);
  language = $state(DEFAULT_LANGUAGE);
  /** Terms to bias recognition toward, as the user typed them. */
  keyterms = $state('');
  maxSeconds = $state(DEFAULT_MAX_SECONDS);

  /**
   * What the backend says is installed, or null before it has been asked.
   *
   * Null rather than an optimistic default: the button is absent until this lands, because a
   * microphone button that reports "install SoX" the first time it is pressed is worse than one
   * that was never offered.
   */
  status = $state<DictationStatus | null>(null);

  /** True between pressing the mic and the transcript arriving. */
  recording = $state(false);
  /** True while the audio is being transcribed, which is after recording and before text. */
  transcribing = $state(false);
  error = $state<string | null>(null);

  /** Whether to draw the button at all. */
  get available(): boolean {
    return this.enabled === 'on' && this.status?.ready === true;
  }

  async init(): Promise<void> {
    try {
      const [enabled, mode, language, keyterms, seconds] = await Promise.all([
        commands.getPref(ENABLED),
        commands.getPref(MODE),
        commands.getPref(LANGUAGE),
        commands.getPref(KEYTERMS),
        commands.getPref(MAX_SECONDS),
      ]);
      this.enabled = enabled === 'on' || enabled === 'off' ? enabled : 'ask';
      this.mode = mode === 'tap' || mode === 'hold' ? mode : DEFAULT_MODE;
      this.language = language?.trim() || DEFAULT_LANGUAGE;
      this.keyterms = keyterms ?? '';
      const parsed = Number(seconds);
      this.maxSeconds =
        Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_MAX_SECONDS;
    } catch {
      // A missing preference is not worth blocking startup over; every default above is the
      // behaviour of a user who has never turned this on.
    }
    // Only worth asking when it might be used. The probe runs three PATH lookups and a keychain
    // read, which is cheap but not free, and a user who has never enabled dictation should not pay
    // for it on every launch.
    if (this.enabled === 'on') await this.refresh();
  }

  /** Re-probe for SoX, curl and a stored key. */
  async refresh(): Promise<void> {
    try {
      this.status = await commands.dictationStatus();
    } catch {
      this.status = null;
    }
  }

  async setEnabled(next: DictateEnabled): Promise<void> {
    this.enabled = next;
    await this.save(ENABLED, next);
    if (next === 'on') await this.refresh();
  }

  async setMode(next: DictateMode): Promise<void> {
    this.mode = next;
    await this.save(MODE, next);
  }

  async setLanguage(next: string): Promise<void> {
    this.language = next.trim() || DEFAULT_LANGUAGE;
    await this.save(LANGUAGE, this.language);
  }

  async setKeyterms(next: string): Promise<void> {
    this.keyterms = next;
    await this.save(KEYTERMS, next);
  }

  async setMaxSeconds(next: number): Promise<void> {
    this.maxSeconds = next;
    await this.save(MAX_SECONDS, String(next));
  }

  /**
   * Store the transcription key.
   *
   * Deliberately takes the value and returns nothing: it goes one way. Nothing reads a key back out
   * of the backend, so this store never holds one and `status.keySet` is all the UI ever learns.
   */
  async setKey(key: string): Promise<void> {
    await commands.setDictationKey(key);
    await this.refresh();
  }

  /** Start recording. Returns false when the backend refused, having set `error`. */
  async start(): Promise<boolean> {
    if (this.recording) return true;
    this.error = null;
    try {
      await commands.startDictation();
      this.recording = true;
      return true;
    } catch (e) {
      this.error = errorMessage(e);
      return false;
    }
  }

  /**
   * Stop recording and return what was said, or null.
   *
   * Null covers both "nothing was recorded" and "the service refused", with `error` carrying which
   * — the caller only needs to know whether there is text to insert.
   */
  async stop(): Promise<string | null> {
    if (!this.recording) return null;
    this.recording = false;
    this.transcribing = true;
    try {
      return (await commands.stopDictation()).trim() || null;
    } catch (e) {
      this.error = errorMessage(e);
      return null;
    } finally {
      this.transcribing = false;
    }
  }

  private async save(key: string, value: string): Promise<void> {
    try {
      await commands.setPref(key, value);
    } catch (e) {
      // Not reverted, for the reason `composerPrefs` gives: the control the user is looking at has
      // already moved, and putting it back under them is the worse surprise.
      this.error = errorMessage(e);
    }
  }
}

export const dictation = new Dictation();
