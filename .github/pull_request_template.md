<!--
Thanks for sending this. Nothing below is mandatory — delete anything that does not apply.
The one genuinely useful section is "Verified by hand"; see below for why.
-->

## What and why

<!-- What changes, and what problem it solves. If there is an issue, link it. -->

## Verified by hand

<!--
CI proves the code compiles, lints and passes tests on both platforms. It proves nothing
about whether the app looks right or whether anything actually launched — nothing in CI
runs the app.

So: what did you actually try? And what did you not? "I did not test this on Linux" is
useful information rather than an admission — nobody has, and saying so is much better
than leaving it ambiguous.
-->

## Checklist

- [ ] `just check` passes
- [ ] Comments explain *why*, where the reasoning is not obvious from the code
- [ ] New tests are named as full sentences, and I can describe how each would fail
- [ ] No new `#[cfg(target_os = …)]` — or if there is, it is in `platform_seams.rs`'s
      `ALLOWED` list with a written reason
