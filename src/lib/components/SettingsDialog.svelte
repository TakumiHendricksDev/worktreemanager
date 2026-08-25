<script lang="ts">
  /**
   * Settings.
   *
   * Four sections. Three of them are preferences that already existed in
   * `~/.config/wtm/config.toml` with no way to reach it from the app — `ui.opener` was written
   * only as a side effect of using the split button, and `exec.path` was file-only despite
   * being the documented fix for the app's most common failure.
   *
   * Notifications is the one exception, and it earns it: `ui.notify` is a tri-state whose
   * unset value means "ask", the asking happens once through a toast, and without a
   * permanent home a user who answered "Not now" would have no way back. It is also the
   * only setting here whose effect is outside the window.
   *
   * # Everything applies immediately
   *
   * No OK button, no Apply, no dirty state. A palette you cannot see until you confirm it is
   * a palette you cannot choose, and once appearance works that way the other two sections
   * behaving differently would be the surprise. The one exception is the PATH field, which
   * saves on blur — see `commitPath`.
   *
   * # Diagnostics are read-only
   *
   * `commands.doctor()` has existed, fully wired, since before this dialog — and nothing
   * called it, while the README's troubleshooting table told people to go and read it. It
   * lives under Advanced because the PATH override directly above is unusable without it:
   * you cannot sensibly override a PATH you cannot see.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Doctor } from '../ipc/types';
  import { attention } from '../state/attention.svelte';
  import { composerPrefs, type SendKey } from '../state/composer.svelte';
  import { DESTINATION, dictation, type DictateMode } from '../state/dictate.svelte';
  import { theme, type ThemeChoice } from '../state/theme.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import Icon from './ui/Icon.svelte';
  import TextInput from './ui/TextInput.svelte';

  const { onclose }: { onclose: () => void } = $props();

  type Section = 'appearance' | 'general' | 'notifications' | 'advanced';
  let section = $state<Section>('appearance');

  const MODES: { value: ThemeChoice; label: string }[] = [
    { value: 'system', label: 'Follow the system' },
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' },
  ];

  /*
   * Each option names *both* keys, because the interesting half of this choice is what happens to
   * the other one. "Enter sends" without "Shift+Enter for a newline" reads like losing the ability
   * to write a second line, which is the thing that makes people not touch the setting.
   */
  const SEND_KEYS: { value: SendKey; label: string; help: string }[] = [
    {
      value: 'mod-enter',
      label: '⌘ Enter sends',
      help: 'Enter starts a new line. Safer for pasting stack traces and diffs.',
    },
    {
      value: 'enter',
      label: 'Enter sends',
      help: 'Shift+Enter starts a new line. ⌘ Enter still sends too.',
    },
  ];

  let doctor = $state<Doctor | null>(null);
  let pathOverride = $state('');
  let pathSaved = $state(false);
  let error = $state<string | null>(null);

  /*
   * Loaded once when the dialog opens rather than when Advanced is first shown.
   *
   * Both are a single read and the dialog is opened deliberately, so the round trip is paid
   * for while the user is still looking at Appearance — which is cheaper than a visible
   * blank panel the moment they click Advanced.
   */
  $effect(() => {
    void (async () => {
      try {
        const [d, stored] = await Promise.all([
          commands.doctor(),
          commands.getPref('exec.path'),
        ]);
        doctor = d;
        pathOverride = stored ?? '';
      } catch (e) {
        error = errorMessage(e);
      }
    })();
  });

  /*
   * The one control that does not save as you type.
   *
   * Every `set_pref` is a read-modify-write of the whole config file, so a keystroke-level
   * save would rewrite it a few dozen times while someone pastes a PATH. On blur is also
   * when a half-typed path stops being half-typed.
   */
  async function commitPath() {
    error = null;
    try {
      await commands.setPref('exec.path', pathOverride.trim());
      pathSaved = true;
    } catch (e) {
      error = errorMessage(e);
    }
  }

  /**
   * The transcription key, as typed. Never as stored.
   *
   * Deliberately not seeded from the backend, because there is no command that would seed it: a
   * stored key is reported as a boolean and never returned. So the field is blank on every open
   * even when a key exists, and the help text below says so rather than leaving a blank field
   * looking like an absent key.
   */
  let dictateKey = $state('');
  let keySaved = $state(false);

  /**
   * Save the key on blur, for the same reason the PATH field does — and one more.
   *
   * Every `set_pref` rewrites the whole config file; this one spawns a keychain process. A
   * keystroke-level save would run one per character of a pasted key.
   */
  async function commitKey() {
    if (dictateKey.trim() === '') return;
    error = null;
    try {
      await dictation.setKey(dictateKey);
      // Cleared rather than left on screen: the value is stored now, and a secret lingering in a
      // form field is a secret in a screenshot.
      dictateKey = '';
      keySaved = true;
    } catch (e) {
      error = errorMessage(e);
    }
  }

  /**
   * Turn dictation on, having said what that means.
   *
   * The one preference in this dialog that asks before applying, which breaks the file header's
   * "everything applies immediately" rule on purpose. That rule exists because a setting you cannot
   * see the effect of is one you cannot choose — it assumes the effect is *visible*. Audio leaving
   * the machine is not, so the consent has to be explicit and has to name where it goes.
   */
  async function toggleDictation(on: boolean) {
    if (!on) {
      await dictation.setEnabled('off');
      return;
    }
    const agreed = window.confirm(
      `Dictation records your microphone and sends the audio to ${DESTINATION} for transcription.\n\n` +
        'This is the only feature in wtm that sends anything off your machine. It needs SoX ' +
        '(brew install sox) and a Deepgram API key.\n\nTurn it on?',
    );
    if (agreed) await dictation.setEnabled('on');
  }

  async function chooseOpener(event: Event) {
    const id = (event.currentTarget as HTMLSelectElement).value;
    await workspace.setPreferredOpener(id);
  }

  /**
   * The six custom properties a user-defined palette needs, as an inline style string.
   *
   * Only for swatches. The *selected* palette is painted on `<html>` by the appearance
   * store; this puts the same six values on one chip so an unselected custom palette can
   * still show what it looks like. A built-in needs none of this — its `data-palette`
   * attribute matches a real rule.
   */
  function customSwatch(id: string): string | undefined {
    const p = theme.customPalettes.find((c) => c.id === id && !c.error);
    if (!p) return undefined;
    const [b3, b4, b5, b6] = p.brand;
    return (
      `--palette-hue:${p.hue};--palette-chroma:${p.chroma};` +
      `--brand-300:${b3};--brand-400:${b4};--brand-500:${b5};--brand-600:${b6}`
    );
  }
</script>

<Dialog title="Settings" {onclose} wide>
  {#snippet body()}
    <nav class="c-tabs" aria-label="Settings sections">
      <button
        class="c-tabs__tab"
        class:is-active={section === 'appearance'}
        onclick={() => (section = 'appearance')}
      >
        Appearance
      </button>
      <button
        class="c-tabs__tab"
        class:is-active={section === 'general'}
        onclick={() => (section = 'general')}
      >
        General
      </button>
      <button
        class="c-tabs__tab"
        class:is-active={section === 'notifications'}
        onclick={() => (section = 'notifications')}
      >
        Notifications
      </button>
      <button
        class="c-tabs__tab"
        class:is-active={section === 'advanced'}
        onclick={() => (section = 'advanced')}
      >
        Advanced
      </button>
    </nav>

    {#if error}
      <p class="c-status--danger">{error}</p>
    {/if}

    {#if section === 'appearance'}
      <div class="o-stack o-stack--loose c-settings__panel">
        <div class="o-stack">
          <h3 class="c-section-heading">Palette</h3>
          <div class="c-settings__palettes">
            {#each theme.all as option (option.id)}
              <button
                class="c-palette"
                class:is-selected={theme.palette === option.id}
                disabled={option.error !== null}
                title={option.error ?? undefined}
                aria-pressed={theme.palette === option.id}
                onclick={() => theme.setPalette(option.id)}
              >
                <!--
                  `data-palette` on the swatch, not just on `<html>`. The palette rules are
                  plain attribute selectors, so the custom properties resolve here too and a
                  Clay chip is Clay while the app is still Pine. A custom palette has no
                  rule to match, so it carries its values inline instead — the same six
                  properties, the same ramp, resolved the same way.
                -->
                <span
                  class="c-palette__chips"
                  data-palette={option.id}
                  style={option.custom ? customSwatch(option.id) : undefined}
                >
                  <span class="c-palette__chip c-palette__chip--bg"></span>
                  <span class="c-palette__chip c-palette__chip--surface"></span>
                  <span class="c-palette__chip c-palette__chip--brand"></span>
                </span>
                <span class="c-palette__name">{option.name}</span>
                {#if theme.palette === option.id}
                  <span class="c-palette__tick"><Icon name="check" size={12} /></span>
                {/if}
              </button>
            {/each}
          </div>
          {#if theme.customPalettes.some((p) => p.error)}
            <p class="c-field__help c-status--warn">
              A palette in your config could not be used. Hover it for the reason.
            </p>
          {:else}
            <p class="c-field__help">
              Define your own in <code>~/.config/wtm/config.toml</code> under
              <code>[ui.palettes]</code>. See the README.
            </p>
          {/if}
        </div>

        <div class="o-stack">
          <h3 class="c-section-heading">Mode</h3>
          {#each MODES as mode (mode.value)}
            <Choice
              type="radio"
              name="wtm-mode"
              checked={theme.choice === mode.value}
              onchange={() => theme.set(mode.value)}
            >
              {mode.label}
            </Choice>
          {/each}
        </div>
      </div>
    {:else if section === 'general'}
      <div class="o-stack o-stack--loose c-settings__panel">
        <div class="o-stack">
          <h3 class="c-section-heading">Send a message with</h3>
          {#each SEND_KEYS as option (option.value)}
            <Choice
              type="radio"
              name="wtm-send-key"
              checked={composerPrefs.sendKey === option.value}
              onchange={() => void composerPrefs.setSendKey(option.value)}
            >
              {option.label}
            </Choice>
          {/each}
          <!-- The selected option's consequence rather than a static sentence, because what this
               setting really decides is which key gets you a newline. -->
          <p class="c-field__help">
            {SEND_KEYS.find((o) => o.value === composerPrefs.sendKey)?.help}
          </p>
        </div>

        <Field
          id="settings-opener"
          label="Open worktrees in"
          help="The tool the Open in… button runs. Picking a different one from that button changes this too."
        >
          <select
            id="settings-opener"
            class="c-select"
            value={workspace.preferredOpener ?? ''}
            onchange={chooseOpener}
            disabled={workspace.openers.length === 0}
          >
            {#each workspace.openers as opener (opener.id)}
              <option value={opener.id} disabled={!opener.available}>
                {opener.label}{opener.available ? '' : '  (not installed)'}
              </option>
            {/each}
          </select>
        </Field>

        <!--
          Dictation. Off unless turned on, and the one control here that asks first — see
          `toggleDictation` for why this section breaks the applies-immediately rule.
        -->
        <div class="o-stack">
          <h3 class="c-section-heading">Dictation</h3>
          <Choice
            type="checkbox"
            checked={dictation.enabled === 'on'}
            onchange={(on) => void toggleDictation(on)}
          >
            Dictate prompts with the microphone
          </Choice>
          <p class="c-field__help">
            The only feature that sends anything off this machine: audio goes to {DESTINATION}
            to be transcribed. Needs SoX and a key, both set up under Advanced.
          </p>

          {#if dictation.enabled === 'on'}
            {#if dictation.status !== null && dictation.status.missing.length > 0}
              <Banner variant="warn">
                Dictation needs {dictation.status.missing.join(' and ')}, which
                {dictation.status.missing.length === 1 ? 'is' : 'are'} not on your PATH.
                {#snippet action()}
                  <code class="u-mono">brew install sox</code>
                {/snippet}
              </Banner>
            {:else if dictation.status !== null && !dictation.status.keySet}
              <Banner variant="warn">
                No transcription key yet — add one under Advanced.
              </Banner>
            {/if}

            <Field
              id="settings-dictate-mode"
              label="Microphone button"
              help="Hold cannot outlive the press, which is why it is the default."
            >
              <select
                id="settings-dictate-mode"
                class="c-select"
                value={dictation.mode}
                onchange={(e) =>
                  void dictation.setMode(
                    (e.currentTarget as HTMLSelectElement).value as DictateMode,
                  )}
              >
                <option value="hold">Hold to talk</option>
                <option value="tap">Tap to start, tap to stop</option>
              </select>
            </Field>

            <Field
              id="settings-dictate-keyterms"
              label="Words to listen for"
              help="Comma separated. Terms the transcriber should expect — the fix for “SDK” coming back as “S D K”."
            >
              <TextInput
                id="settings-dictate-keyterms"
                value={dictation.keyterms}
                placeholder="SDK, ChatGPT, Tauri, argv, PTY"
                onblur={(e) =>
                  void dictation.setKeyterms((e.currentTarget as HTMLInputElement).value)}
              />
            </Field>
          {/if}
        </div>
      </div>
    {:else if section === 'notifications'}
      <div class="o-stack o-stack--loose c-settings__panel">
        <!--
          The permanent home for a preference that is otherwise only ever offered once, by a toast on
          the first focus after something was missed — so this is also the recovery path for anyone who
          answered "Not now" and later wanted them.

          Applies immediately, like everything else here. It needs no equivalent of the PATH field's
          blur exception: that one saves on blur because every `set_pref` rewrites the whole config
          file and a keystroke-level save would do it dozens of times, where this is one click.
        -->
        <Choice
          type="checkbox"
          checked={attention.pref === 'on'}
          onchange={() =>
            void (attention.pref === 'on' ? attention.disable() : attention.enable())}
        >
          Notify me when a session needs attention
        </Choice>
        <p class="c-field__help">
          Only while wtm is in the background — a session in the window says so itself, in
          the pane and in the sidebar. Nothing is ever sent about the worktree you are
          looking at.
        </p>
        {#if attention.blocked}
          <!-- Said rather than swallowed: a preference that is on and silent is indistinguishable
               from a broken app, and the fix is somewhere wtm cannot reach. -->
          <p class="c-field__help c-status--warn">
            macOS is not delivering wtm's notifications. Turn them on for Worktree Manager
            in System Settings → Notifications.
          </p>
        {/if}
      </div>
    {:else}
      <div class="o-stack o-stack--loose c-settings__panel">
        <Field
          id="settings-path"
          label="PATH override"
          help="Leave empty to use the PATH probed from your login shell. Takes effect when wtm restarts."
          errors={[error]}
        >
          <TextInput
            id="settings-path"
            bind:value={pathOverride}
            mono
            placeholder="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
            oninput={() => (pathSaved = false)}
          />
        </Field>
        <div class="c-settings__row">
          <Button variant="neutral" size="sm" onclick={commitPath}>Save PATH</Button>
          {#if pathSaved}
            <span class="c-status--ok">Saved — restart wtm to use it.</span>
          {/if}
        </div>

        <!--
          The transcription key.

          Beside the PATH override because both are machine-level setup rather than taste, and both
          are things you set once. It is blank on every open even when a key is stored: nothing
          reads a key back out of the backend, which is the point rather than an omission.
        -->
        <Field
          id="settings-dictate-key"
          label="Transcription key"
          help={dictation.status?.keySet === true
            ? 'A key is stored in your keychain. Type a new one to replace it, or save an empty field to remove it.'
            : `A Deepgram API key, kept in your keychain and sent only to ${DESTINATION}.`}
        >
          <TextInput
            id="settings-dictate-key"
            bind:value={dictateKey}
            mono
            placeholder={dictation.status?.keySet === true ? '••••••••' : 'Paste a key'}
            oninput={() => (keySaved = false)}
            onblur={commitKey}
          />
        </Field>
        <div class="c-settings__row">
          <Button variant="neutral" size="sm" onclick={commitKey}>Save key</Button>
          {#if dictation.status?.keySet === true}
            <Button
              variant="quiet"
              size="sm"
              onclick={() => void dictation.setKey('').then(() => (keySaved = false))}
            >
              Remove
            </Button>
          {/if}
          {#if keySaved}
            <span class="c-status--ok">Saved to your keychain.</span>
          {/if}
        </div>

        {#if doctor}
          <div class="o-stack">
            <h3 class="c-section-heading">Diagnostics</h3>
            <dl class="o-facts">
              <dt>PATH in use</dt>
              <dd><code>{doctor.resolvedPath}</code></dd>
              <dt>Source</dt>
              <dd>{doctor.pathSource}</dd>
              <dt>Config</dt>
              <dd><code>{doctor.configDir}</code></dd>
            </dl>
          </div>

          <div class="o-stack">
            <h3 class="c-section-heading">Tools on that PATH</h3>
            <dl class="o-facts">
              {#each doctor.tools as tool (tool.name)}
                <dt>{tool.name}</dt>
                <dd>
                  {#if tool.path}
                    <code>{tool.path}</code>
                  {:else}
                    <span class="c-status--muted">not found</span>
                  {/if}
                </dd>
              {/each}
            </dl>
          </div>
        {/if}
      </div>
    {/if}
  {/snippet}

  {#snippet footer()}
    <Button variant="neutral" onclick={onclose}>Done</Button>
  {/snippet}
</Dialog>
