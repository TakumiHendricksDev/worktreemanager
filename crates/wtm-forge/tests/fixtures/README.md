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

The `review_*` replies are what `gh` 2.100.0 printed on 2026-10-21 for a throwaway private pull
request, with its repository, account and commit author replaced the same way (`acme/webapp`,
`dana-lee`). Where GitHub refused, `.json` is the stdout and `.stderr` is `gh`'s one line.

- `review_create_pending.json`: `gh api --method POST repos/{o}/{r}/pulls/{n}/reviews --input -`
  with no `event`, which starts a pending review.
- `review_pending_exists.*`: the same request while a pending review exists (HTTP 422, "User can
  only have one pending review per pull request").
- `review_line_unresolved.*`: the same request with a comment on a line the diff does not show
  (HTTP 422, "Line could not be resolved").
- `review_thread_added.json`, `review_thread_null.json`, `review_thread_bad_node.*`: GraphQL
  `addPullRequestReviewThread` on a pending review: added; a line outside the diff, which GitHub
  answers with `thread: null` and no error; and a review id that does not exist.
- `review_submitted.json`: `POST …/reviews/{id}/events` with `event: COMMENT`.
- `review_reviews.json`, `review_review_comments.json`: `gh api --method GET --paginate --slurp`
  of `…/reviews` and `…/reviews/{id}/comments`, with markers in the bodies.
- `review_compare.json`: `gh api --method GET repos/{o}/{r}/compare/{base}...{head}`.

The `pull*` replies are what `gh` 2.100.0 printed on 2026-10-21 for the same throwaway repository,
named the same way, for the follow-up pull request of a local fix.

- `pulls_by_head.json`: `gh api --method GET --paginate --slurp
  repos/{o}/{r}/pulls?head={owner}:{branch}&state=all`, finding one open pull request.
- `pull_created.json`: that reply's one pull request, alone. `POST repos/{o}/{r}/pulls` answers
  with the same object, which is how the create's reply is read.
- `pull_exists.*`: `POST repos/{o}/{r}/pulls` from a head that already has a pull request into
  that base (HTTP 422, "A pull request already exists for …"). Nothing was opened.
