# `gh` replies

JSON as `gh` 2.100.0 printed it on 2026-10-20, for the commands `src/gh.rs` runs, with
the repositories, people and ids replaced by placeholders (`acme/webapp`, a fork at
`sam-rivera/webapp`). Field names, nesting, enum spellings and the `null`s are `gh`'s own.

- `pr_same_repo.json`: `gh pr view <n> --json <PR_FIELDS>`, a merged pull request from a branch
  of the base repository, every check passed.
- `pr_fork.json`: the same command, an open pull request from a fork whose branch is called
  `main`, with a user and a team asked to review and a mix of check runs and commit statuses.
- `pr_sparse.json`: the least `gh` could plausibly print, plus a state and a check kind nobody
  has seen and a field nobody asked for, to hold the parser to "tolerant".
- `pr_list_head.json`: `gh pr list --head <branch> --state all --json <PR_FIELDS>`, where a fork
  shares the branch name.
- `repo_permission.json`: `gh repo view <repo> --json viewerPermission`.
- `branch.json`, `branch_rules.json`: `gh api --method GET` on `repos/{o}/{r}/branches/{b}` and
  `repos/{o}/{r}/rules/branches/{b}` for a branch protected by a ruleset, not by classic
  protection.
- `auth_status.json`, `auth_status_bad_token.json`, `auth_status_none.json`:
  `gh auth status --hostname github.com --json hosts` signed in, with a rejected token, and with
  no account at all.
