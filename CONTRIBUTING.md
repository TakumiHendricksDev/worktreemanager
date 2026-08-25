# Contributing

Thanks for looking. This is a small project with strong opinions about a few things and no
opinion at all about most things — this file is mostly about telling those two groups apart,
so you do not have to guess.

## Before a big change, open an issue

Small fixes: just send the PR. Anything that adds a dependency, a platform seam, or a new
concept to the domain: please open an issue first. Not for ceremony — the architecture below
makes some changes much more expensive than they look, and it is better to find that out in a
paragraph than in a rejected pull request.

## Getting set up

You need [Rust](https://rustup.rs) (the version in `rust-toolchain.toml` is pinned exactly and
rustup will install it for you), [bun](https://bun.sh), and [just](https://just.systems).
On Linux you also need the WebKitGTK development packages — see
[Prerequisites](README.md#prerequisites).

```bash
just setup     # installs frontend deps and the pre-commit hook
just dev       # the app, with hot reload
just check     # everything CI runs
```

The first build compiles ~800 crates and takes several minutes with long silences on `tao`,
`wry` and `objc2-app-kit`. It is not hung.

## The one gate

```bash
just check
```

That is `cargo fmt --check`, `prettier --check`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, `svelte-check --fail-on-warnings`, `cargo nextest run
--workspace`, and a `wasm32` check of `wtm-core`. CI runs exactly these commands, so if it
passes locally it passes there. If it fails locally and you cannot tell why, say so in the PR
and push anyway — a confusing lint message is a reasonable thing to ask about.

`just audit` (cargo-deny plus `bun audit`) is deliberately *not* part of `just check`, because
it hits the network. CI runs it separately.

## Things that will fail the build, and why they exist

These are enforced rather than requested, so it is worth knowing about them before you write
code that trips them.

**`std::process::Command::new` is banned** outside `wtm-exec`. Every subprocess in the app
funnels through one place so that it gets a deadline, a sanitized environment, a resolved
`PATH` and a tracing span. There is one exception with no deadline —
`Runner::launch_detached`, for handing a directory to a GUI application — and its doc comment
explains why at length. Read that before adding a second one.

**`SystemTime::now` and `Instant::now` are banned** outside the clock adapter. Use-cases take
a `Clock` port so they are testable at a fixed instant.

**`std::sync::Mutex` is banned** in favour of `parking_lot::Mutex` — no poisoning, so no
`unwrap()` on every lock.

**`unsafe` is `forbid`den**, workspace-wide. This is why the project depends on `nix` rather
than `libc`. One crate sits outside the workspace table for a mechanical reason: `forbid`
cannot be relaxed by the crate it applies to, so `wtm-notify` — which exists to fence the
objc2 FFI a click-navigating macOS notification needs — restates that table verbatim with
`unsafe_code = "deny"`. Its `Cargo.toml` header names the safe wrappers that were rejected and
why. A second such crate is an open-an-issue-first change.

**Exactly one thing in this repository reaches the network, and it is not the webview.**
Dictation sends recorded audio to a host that is a `const` in `wtm-dictate`, by invoking `curl`.
`src-tauri/tests/network_boundary.rs` pins three properties: the CSP still permits the frontend no
network destination at all, that host stays a constant rather than becoming configuration, and no
crate in the workspace declares an HTTP client. A second network call, or reaching for `reqwest`,
is an open-an-issue-first change — ARCHITECTURE §6a records why `curl` was chosen over a linked TLS
stack, and what it costs.

**`wtm-core` must compile for `wasm32-unknown-unknown`.** That is not because anyone runs it
in a browser; it is a mechanical proof that the domain has no operating-system dependency. If
your change makes that check fail, the logic belongs in an adapter.

**`#[cfg(target_os = …)]` is capped at two files**, enforced by
`src-tauri/tests/platform_seams.rs`. A platform seam is warranted only where the other
platform's code cannot compile or cannot be expressed. `open` vs `xdg-open` qualifies;
`fs::metadata("/Applications/Zed.app")` does not — it compiles everywhere and answers
correctly everywhere. Prefer data, or a runtime `cfg!()`, so both arms stay under test on both
runners. If you genuinely need a third seam, add it to that test's `ALLOWED` list *with the
reason written next to it*.

**No project-specific identifiers**, enforced by `src-tauri/tests/repo_hygiene.rs`. wtm is
meant to work for any repository, and its source should not know about any particular one.
Use a neutral placeholder in fixtures and docs.

## The CSS rules, which are reviewed rather than enforced

Every rule above names the test that enforces it. These have no test, and saying so is more
useful than implying a gate that does not exist — there is no stylelint here, and since the
stylesheet went global there are no unused-selector warnings either. They are review items.

- **No `<style>` block in a component.** All styles are global, in `src/styles/`.
- **Class names are BEMIT**: `.o-` object, `.c-` component, `.u-` utility, `.is-`/`.has-` for
  runtime state — and a state class is always chained (`.c-tab.is-selected`), never styled
  alone.
- **No naked element selector outside `elements/`.** Inside `components/` and `objects/` an
  element may only appear as a descendant of a block class. `h2` meant two different things in
  five files before this rule; scoping hid it, and going global would not have.
- **No selector over two compound parts, and no `&`-nesting deeper than one level.**
- **No `@extend`** — it hoists selectors to the extended rule's position and silently destroys
  the layer order. Use a mixin.
- **No `!important` outside `utilities/`**, with two documented exceptions carrying their
  reasons: the reduced-motion block and the progress bar's indeterminate width.
- **No literal colour and no layer-1 primitive in a component.** `--danger`, not `--red-500`,
  and not `#c8393a`. If you need a tinted surface, there is a `-soft`/`-line` role for it —
  reaching for a raw `color-mix()` percentage is how eight different percentages accumulated
  across five files before.

`src/styles/main.scss` states the layer order and what breaks if you reorder it. Read that
header before adding a file to it.

## Architecture, in one paragraph

`wtm-core` is the domain: models, ports, use-cases, and no I/O. Everything that touches the
world is an adapter behind a port — `wtm-git` shells out to `git`, `wtm-exec` runs processes
and resolves `PATH`, `wtm-config` reads the layered TOML and the trust store, `wtm-render`
is the template engine. `src-tauri` is the composition root: the only place a concrete
adapter is chosen, and the only place allowed to hold an opinion about the UI. If you find
yourself wanting to put a UI concern on a port, that is the signal to put it in `src-tauri`
instead — see the comment on `FileConfigStore::favorites` for the canonical example.

[ARCHITECTURE.md](ARCHITECTURE.md) has the long version.

## Style

**Comments explain *why*, not *what*.** The code says what it does. A comment earns its place
by recording a constraint, a rejected alternative, or a bug that motivated the shape of
something. This is the most distinctive thing about the codebase and the easiest thing to get
wrong — read a couple of files before writing new ones.

**Test names are full sentences** describing the property being proved:
`a_star_survives_a_new_app_reading_the_same_config`, not `test_favorites`. Every test file
opens with a `//!` block explaining what it protects and why it exists.

**Prefer a test that can fail.** If you cannot describe how a test would go red, it is
probably not testing anything. Several tests here were verified by deliberately breaking the
code and watching them fail; that is a good habit.

**Commit messages are prose.** Subject line in the imperative, then paragraphs explaining the
reasoning — what was wrong, what was considered, why this shape. `git log` is the primary
record of *why* the code looks the way it does, so it is worth writing properly.

## Pull requests

- Branch off `main`.
- Keep the change focused. Unrelated cleanups are welcome, in their own commits.
- Make sure `just check` passes. All five CI checks must be green before a PR can merge.
- Describe what you verified by hand, and what you did not. "I did not test this on Linux" is
  useful information, not an admission — nobody has, and saying so is better than implying
  otherwise.

## Licensing

By contributing you agree that your contributions are licensed under the
[MIT License](LICENSE), the same as the rest of the project.
