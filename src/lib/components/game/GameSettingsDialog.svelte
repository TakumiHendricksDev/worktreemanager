<script lang="ts">
  /** Renderer-only preferences. They stay local to game mode and apply immediately. */
  import type { RenderSettings } from '../../game/vendor';
  import Button from '../ui/Button.svelte';
  import Choice from '../ui/Choice.svelte';
  import Dialog from '../ui/Dialog.svelte';
  import Field from '../ui/Field.svelte';

  const {
    settings,
    onclose,
  }: {
    settings: RenderSettings;
    onclose: () => void;
  } = $props();

  // The vendored settings object publishes its own change event rather than using Svelte state.
  // Bumping this counter is the small adapter that makes immediate changes repaint the controls.
  let revision = $state(0);
  $effect(() => settings.onChange(() => (revision += 1)));

  const PRESETS = [
    { id: 'potato', label: 'Potato', hint: 'Battery first — flat light, no extras' },
    { id: 'low', label: 'Low', hint: 'For when you are on the go' },
    { id: 'balanced', label: 'Balanced', hint: 'The default — looks good, runs cool' },
    { id: 'high', label: 'High', hint: 'Sharp shadows and a full sky' },
    { id: 'ultra', label: 'Ultra', hint: 'Everything on, plugged in' },
  ] as const;

  function value<T>(key: string): T {
    void revision;
    return settings.get(key) as T;
  }
</script>

<Dialog title="Game Mode settings" {onclose} wide>
  {#snippet body()}
    <div class="o-stack o-stack--loose c-settings__panel">
      <div class="o-stack">
        <h3 class="c-section-heading">Quality</h3>
        <div class="c-game-settings__presets">
          {#each PRESETS as preset (preset.id)}
            <Button
              variant={value<string>('preset') === preset.id ? 'accent' : 'neutral'}
              size="sm"
              title={preset.hint}
              onclick={() => settings.applyPreset(preset.id)}>{preset.label}</Button
            >
          {/each}
        </div>
        <Field
          id="game-render-scale"
          label="Render scale"
          help="Adaptive quality may lower this temporarily when frames get expensive."
        >
          <input
            id="game-render-scale"
            class="c-game-settings__range"
            type="range"
            min="0.35"
            max="1.5"
            step="0.05"
            value={value<number>('renderScale')}
            oninput={(event) =>
              settings.set(
                'renderScale',
                Number((event.currentTarget as HTMLInputElement).value),
              )}
          />
        </Field>
        <Choice
          type="checkbox"
          checked={value<boolean>('autoQuality')}
          onchange={(checked) => settings.set('autoQuality', checked)}
          >Adaptive quality</Choice
        >
        <Choice
          type="checkbox"
          checked={value<boolean>('bloom')}
          onchange={(checked) => settings.set('bloom', checked)}>HDR and bloom</Choice
        >
        <Choice
          type="checkbox"
          checked={value<boolean>('tiltShift')}
          onchange={(checked) => settings.set('tiltShift', checked)}
          >Tilt-shift depth</Choice
        >
      </div>

      <div class="o-stack">
        <h3 class="c-section-heading">World</h3>
        <Field id="game-planet" label="Environment">
          <select
            id="game-planet"
            class="c-select"
            value={value<string>('planet')}
            onchange={(event) =>
              settings.set('planet', (event.currentTarget as HTMLSelectElement).value)}
          >
            <option value="terra">Terra archipelago</option>
            <option value="moon">Lunar colony</option>
            <option value="mars">Martian colony</option>
          </select>
        </Field>
        <Choice
          type="checkbox"
          checked={value<boolean>('showLabels')}
          onchange={(checked) => settings.set('showLabels', checked)}
          >Repository labels</Choice
        >
        <Choice
          type="checkbox"
          checked={value<boolean>('autoFrame')}
          onchange={(checked) => settings.set('autoFrame', checked)}
          >Return to isometric after moving</Choice
        >
        <Choice
          type="checkbox"
          checked={value<boolean>('reducedMotion')}
          onchange={(checked) => settings.set('reducedMotion', checked)}
          >Reduced motion</Choice
        >
      </div>
    </div>
  {/snippet}
  {#snippet footer()}
    <Button variant="neutral" onclick={onclose}>Done</Button>
  {/snippet}
</Dialog>
