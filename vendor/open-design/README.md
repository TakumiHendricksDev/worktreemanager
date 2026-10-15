# Open Design subset used by WTM

Source: nexu-io/open-design, revision `7da9a8aaa2d406bf3aa0a1cb9d4e8e1cd24c9095`
(version 0.25.0), Apache-2.0. See LICENSE and provenance.json.

WTM embeds `design-charter.md`, an explicitly adapted excerpt of
`apps/daemon/src/prompts/core-slim.ts`: role, planning, reuse, verification and
refinement. WTM supplies its own host contract for tool delivery, project evidence,
self-contained artifacts, preview and user approval. The upstream form wizard,
CDN delivery, CLI export commands and separate provider runtime are not included.

`token-evidence.ts` is the unmodified upstream reference, **not executed or imported
by WTM**. `src-tauri/src/design_evidence.rs` is a Rust adaptation of its CSS custom
property, literal-color, typography, spacing, radius and shadow collectors. It
keeps original source/line references, uses a bounded flat report, and does not
include upstream's Swift/Tailwind path handling, grouped usage reports, token
contract rebuilding, catalog or default token fallbacks. The agent receives the
actual component source alongside this report; WTM does not claim to mount Vue,
React or Svelte components in an HTML prototype.

These assets are compiled into WTM; the attribution, license and reference sources
also ship in the app's `open-design` resources. No first-use download, Node runtime,
Open Design service, provider login, cloud connection or global configuration is
needed. Regenerate the subset from a checkout of the pinned revision with:

```sh
python3 scripts/vendor-open-design.py /path/to/open-design
```

The script checks source hashes before writing. Builds use these checked-in assets
and require no upstream checkout or network access.
