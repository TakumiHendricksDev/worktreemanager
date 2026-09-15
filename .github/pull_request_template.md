<!--
Thanks for sending this. Nothing below is mandatory — delete anything that does not apply.
The one genuinely useful section is "Verified by hand"; see below for why.
-->

## What and why

<!-- What changes, and what problem it solves. If there is an issue, link it. -->

## Verified by hand

<!--
CI proves the code compiles, lints, passes tests and bundles into a .app. It proves
nothing about whether the app looks right or whether anything actually launched —
nothing in CI runs the app.

So: what did you actually try? And what did you not? "I did not click through the
browser pane" is useful information rather than an admission, and much better than
leaving it ambiguous.
-->

## Checklist

- [ ] `just check` passes
- [ ] Comments explain *why*, where the reasoning is not obvious from the code
- [ ] New tests are named as full sentences, and I can describe how each would fail
- [ ] No new `#[cfg(target_os = …)]` — or if there is, it is in `platform_seams.rs`'s
      `ALLOWED` list with a written reason
