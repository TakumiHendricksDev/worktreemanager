<script lang="ts">
  /**
   * Settings → Plugins → Workbench defaults: which agent experience new sessions open in, and what
   * fills each workbench region.
   *
   * A region change applies at once, like every other setting, except hiding one of the regions
   * in `HIDING_ASKS`. Hiding the whole workbench, the window's navigation or the sessions leaves a
   * window with no visible way back, so that one choice is confirmed first and the confirmation
   * says how to return. The select shows the value being decided while it is asked about or saved,
   * and goes back to the registry's answer if it is cancelled or refused: a native select keeps
   * whatever was picked unless its bound value changes, which a refusal never does.
   */
  import {
    HIDING_ASKS,
    PLUGIN_SLOTS,
    type PluginBinding,
    type PluginSlot,
  } from '../plugins';
  import { plugins, PluginTask } from '../state/plugins.svelte';
  import Banner from './ui/Banner.svelte';
  import Field from './ui/Field.svelte';
  import ConfirmPluginAction from './ConfirmPluginAction.svelte';

  const { onnotice }: { onnotice: (message: string) => void } = $props();

  type Experience = 'wtm.chat' | 'wtm.native-cli';
  const EXPERIENCES: { id: Experience; name: string; summary: string }[] = [
    {
      id: 'wtm.chat',
      name: 'WTM Chat',
      summary: 'wtm’s transcript, tools, approvals and delegation.',
    },
    {
      id: 'wtm.native-cli',
      name: 'Native CLI',
      summary: 'The vendor’s terminal interface, commands and permission prompts.',
    },
  ];

  /**
   * The question, and what is left on screen, for each region in `HIDING_ASKS`. The workbench and
   * navigation leave `WorkbenchRecoveryBar`, which the confirmation names as the way back.
   */
  const HIDING: Partial<Record<PluginSlot, { title: string; leaves: string }>> = {
    workbench: {
      title: 'Hide the entire workbench?',
      leaves:
        'The window shows only a strip saying Workbench hidden, with Plugins & recovery to bring you back here.',
    },
    titlebar: {
      title: 'Hide window navigation?',
      leaves:
        'Project navigation and the window toolbar go. A strip at the top keeps Plugins & recovery, which brings you back here.',
    },
    sessions: {
      title: 'Hide the session workbench?',
      leaves:
        'Sessions keep running, but no pane shows them, so you cannot read or answer one until it is back.',
    },
  };

  const task = new PluginTask();
  /** A region's value while it is being asked about or saved. See the header. */
  const pending = $state<Partial<Record<PluginSlot, string>>>({});
  let hiding = $state<PluginSlot | null>(null);

  function choiceFor(slot: PluginSlot): string {
    const b = plugins.binding(slot);
    return b.kind === 'plugin' ? `${b.plugin}:${b.view}` : b.kind;
  }

  function bindingFor(value: string): PluginBinding {
    if (value === 'builtin' || value === 'hidden') return { kind: value };
    const [plugin = '', view = ''] = value.split(':');
    return { kind: 'plugin', plugin, view };
  }

  async function bind(slot: PluginSlot, value: string) {
    pending[slot] = value;
    await task.run(() => plugins.bind(slot, bindingFor(value)));
    delete pending[slot];
  }

  function choose(slot: PluginSlot, value: string) {
    if (value === 'hidden' && HIDING_ASKS.has(slot)) {
      pending[slot] = value;
      hiding = slot;
    } else void bind(slot, value);
  }

  function cancelHiding() {
    if (hiding) delete pending[hiding];
    hiding = null;
  }

  async function experience(id: Experience, name: string) {
    if (await task.run(() => plugins.setExperience(id)))
      onnotice(`New local agent sessions open in ${name}.`);
  }
</script>

<div class="c-plugins__content o-stack o-stack--loose">
  <h3 class="c-pane-title">Choose how you work</h3>
  <p class="u-muted">Choose where sessions run and how you interact with them.</p>
  {#if task.error}<Banner>{task.error}</Banner>{/if}
  <div class="c-plugins__section o-stack">
    <h4 class="c-section-heading">Session host</h4>
    <p>This computer</p>
    <p class="c-note">
      Installed session backends offer their own hosts through Browse sessions. Attaching to
      an existing session keeps its host.
    </p>
  </div>
  <div
    class="c-plugins__section o-stack"
    role="group"
    aria-labelledby="plugin-experience-heading"
  >
    <h4 class="c-section-heading" id="plugin-experience-heading">Local agent experience</h4>
    <div class="o-row c-plugins__choices">
      {#each EXPERIENCES as option (option.id)}<button
          class="c-plugins__choice"
          class:is-selected={plugins.state.registry.experience === option.id}
          aria-pressed={plugins.state.registry.experience === option.id}
          disabled={task.busy || !plugins.enabled(option.id)}
          onclick={() => void experience(option.id, option.name)}
          ><strong>{option.name}</strong><span class="c-note">{option.summary}</span
          ></button
        >{/each}
    </div>
    <p class="c-note">
      Native CLI uses the installed claude or codex command directly. Structured approvals,
      delegation and turn status stay features of WTM Chat. Sessions already open keep their
      experience.
    </p>
  </div>
  <div class="c-plugins__section o-stack">
    <h4 class="c-section-heading">Region replacements</h4>
    <p>Installing a plugin never replaces a region on its own; you choose it here.</p>
    {#each Object.entries(PLUGIN_SLOTS) as [key, label]}
      {@const slot = key as PluginSlot}
      <Field id={`plugin-slot-${slot}`} {label}>
        <select
          id={`plugin-slot-${slot}`}
          class="c-input"
          value={pending[slot] ?? choiceFor(slot)}
          disabled={task.busy || hiding !== null}
          onchange={(e) => choose(slot, e.currentTarget.value)}
        >
          <option value="builtin">Built-in</option><option value="hidden">Hidden</option>
          {#each Object.values(plugins.state.registry.installed) as p}
            {#if plugins.enabled(p.current.manifest.id)}
              {#each p.current.manifest.views.filter((v) => v.slots.includes(slot)) as v}
                <option value={`${p.current.manifest.id}:${v.id}`}
                  >{p.current.manifest.name} · {v.title}</option
                >
              {/each}
            {/if}
          {/each}
        </select>
      </Field>
    {/each}
    <p class="c-note">
      A replacement owns its region’s screen. Settings, permission checks and recovery stay
      with wtm. Turning off or uninstalling a plugin puts the built-in back. Hiding a region
      keeps its data and its running sessions.
    </p>
  </div>
</div>

{#if hiding}
  {@const slot = hiding}
  <ConfirmPluginAction
    title={HIDING[slot]?.title ?? `Hide ${PLUGIN_SLOTS[slot]}?`}
    confirm="Hide"
    working="Hiding…"
    action={() => plugins.bind(slot, { kind: 'hidden' })}
    ondone={() => {
      // Read before `hiding` is cleared: `slot` is derived from it.
      const hidden = slot;
      delete pending[hidden];
      hiding = null;
      onnotice(`${PLUGIN_SLOTS[hidden]} is hidden.`);
    }}
    onclose={cancelHiding}
  >
    <p>{HIDING[slot]?.leaves}</p>
    <p>
      To get it back, press ⌘, to open Settings and choose Built-in for this region in
      Plugins → Workbench defaults, or use Plugin recovery at the bottom of Plugins.
    </p>
  </ConfirmPluginAction>
{/if}
