<script lang="ts">
  /**
   * A session's state, as a dot.
   *
   * # Why the prop is a union and not a string
   *
   * Because the class name is interpolated from it. The stylesheet is global, so there are no
   * unused-selector warnings, and `svelte-check` cannot see that `c-dot--workign` matches nothing —
   * a typed prop is the only mechanism in this codebase that catches it. The same contract every
   * other component in this directory states, and the reason they all state it.
   *
   * # The dot is never the only signal
   *
   * `settings/_semantic.scss` forbids state encoded in colour alone, and a coloured dot in a list is
   * the case that rule most obviously exists for. So there are three channels: the hue, a **shape**
   * (hollow, filled, pulsing, ringed — see `_dot.scss`), and a **word**. Both places a dot appears
   * today have the word beside it, which is why `labelled` defaults to false: naming the dot where a
   * word is already on screen would make a screen reader read the state twice.
   *
   * That is exactly `Icon.svelte`'s contract, and deliberately so — a decorative graphic beside its
   * own label is the same problem both times. The `title` is set either way, because a tooltip on
   * hover is useful even when the state is already written next to it.
   */
  import { STATUS_NAME, type PaneStatus } from '../../status';

  const {
    status,
    labelled = false,
    name,
    steady = false,
  }: {
    status: PaneStatus;
    /**
     * Whether the dot carries its own accessible name.
     *
     * Leave it false when a word is rendered beside the dot — the dot is then `aria-hidden` and the
     * word is what a screen reader reads. Set it true only where the dot stands alone.
     */
    labelled?: boolean;
    /**
     * The tooltip and accessible name, for a dot that is not a session's. A worktree job borrows a
     * session state's shape, but "finished, not seen yet" is not what a created worktree is.
     */
    name?: string;
    /**
     * Held still, for a heading that sums up the rows under it. Each dot's pulse starts when its row
     * mounts, so two summaries of the same state pulsed out of step and read as two different
     * colours; the live state is the session's own row, which still pulses.
     */
    steady?: boolean;
  } = $props();

  const label = $derived(name ?? STATUS_NAME[status]);
</script>

<span
  class="c-dot c-dot--{status}"
  class:c-dot--steady={steady}
  title={label}
  role={labelled ? 'img' : undefined}
  aria-label={labelled ? label : undefined}
  aria-hidden={labelled ? undefined : 'true'}
></span>
