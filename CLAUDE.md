# Notes for AI agents working on wtm

This file is an index, not a rulebook. The rules live where the people who follow them already
look, and restating them here is how the two copies drift apart.

**Read these first:**

- [CONTRIBUTING.md](CONTRIBUTING.md) — the one gate (`just check`), the things that will fail
  the build and why they exist, and the style rules. The comments-explain-*why* rule and the
  full-sentence test names are the two most distinctive things about this codebase and the two
  easiest to get wrong.
- [ARCHITECTURE.md](ARCHITECTURE.md) — the *why* behind the structure. §7 is toolchain policy,
  §8 is the frontend and the CSS architecture, §9 is what this project deliberately does not do.

## The short version of the CSS rules

Styles live in `src/styles/`, and `src/main.ts` imports exactly one file: `styles/main.scss`.
Its `@use` order **is** the architecture — ITCSS layers, specificity climbing monotonically —
and that file's header explains what breaks if you reorder it.

**Components carry class names only. There are no `<style>` blocks in `.svelte` files.**

```
.o-block   object     geometry only, no cosmetics
.c-block   component  a named thing with a look
.u-thing   utility    one property; the only layer allowed !important
.is-/.has- state      runtime, toggled by class:, ALWAYS chained (.c-tab.is-selected)
```

Three limits you can check by eye: no selector over **two** compound parts; no `&`-nesting
deeper than **one** level; and outside `elements/`, an element selector may only appear as a
descendant of a block class (`.o-facts > dt` is fine, a bare `dt` is not).

Colour values come from layer-2 semantic tokens — `--danger`, `--danger-soft` — never from a
layer-1 primitive like `--gray-300` and never from a literal.

## The thing most likely to trip you up

There is **no JS test runner**, and since the stylesheet went global there are no unused-CSS
warnings either. `svelte-check` cannot see a wrong class name. The only mechanism that still
catches one is a **typed component prop** — `variant: 'accent' | 'neutral' | …`,
`name: IconName` — which is why the UI components in `src/lib/components/ui/` express their
class contracts as union types rather than accepting arbitrary strings. Keep it that way.

If you change anything visual, say plainly in the PR what you actually clicked through and
what you did not. `.github/pull_request_template.md` explains why that is more useful than a
ticked box.
