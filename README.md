# P2 click-through screenshots

Screenshots for the P2 pull request (publishing a review through the outbox). Not code; an orphan
branch so they never reach `main`.

- `live/`: the real app, `just dev-isolated ~/.cache/wtm-p2-publish`, publishing real reviews on the
  throwaway private pull request TakumiHendricksDev/wtm-p2-publish-fixture#2. Numbered in the order
  they were taken. Clicks were DOM `element.click()` through a local Vite driver, not OS input.
- `harness/`: `scripts/check-publish-ui.mjs`, mocked IPC, light and dark, for the states that were
  too brief to catch live (Checking GitHub after a relaunch) and the inbox card.
