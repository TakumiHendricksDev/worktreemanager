# `acli` replies

What `acli` 1.3.18 printed on 2026-10-21, against a Jira Cloud site, for the commands `src/acli.rs`
runs. Keys, names, nesting, `null`s, enum spellings, dates and the way pages follow one another are
`acli`'s own; everything that could identify the site, its project or its people was replaced:

- the site's hosts became `acme.atlassian.net`, avatar URLs `https://avatar.example/…`, and links
  inside descriptions `https://example.com/doc`;
- issue keys became `ACME-1nn`, and keys of another project on the same site `OPS-1nn`;
- summaries became `Example issue N`, rich-text words `Placeholder text.`, people one of four
  invented names, account ids zeros, labels `export`/`backend`;
- the team's status names became neutral ones (`In Review`, `Ready`, `Refinement`) and sprint names
  `Sprint 41`–`43`; the site's custom field ids became `customfield_10020` (sprints) and
  `customfield_10050` (a user picker);
- the active sprint's goal, empty on the day, was set to "Ship the bulk export", so a goal is read
  from a real reply's shape. The future sprints keep the empty goal the tracker really sends.

The scrub kept every key it did not replace, so a field the parser ignores is still there for it
to ignore.

- `version.txt`: `acli --version`, upgrade notice included.
- `auth_status.txt`: `acli jira auth status`, signed in. Text, not JSON.
- `search.json`: `acli jira workitem search --jql=… --fields=key,summary,status,assignee,issuetype,priority,labels --limit=4 --paginate --json`.
  One array.
- `sprint_list_workitems.json`:
  `acli jira sprint list-workitems --sprint=… --board=… --jql=… --fields=… --limit=40 --paginate --json`.
  Two pages, printed one after the other, each cut to three issues; `total` and `startAt` are as
  printed. Note what is missing: the labels asked for are not returned.
- `board_list_sprints.json`: `acli jira board list-sprints --id=… --state=active,future --json`.
- `sprint_view.json`: `acli jira sprint view --id=… --json`.
- `workitem_view.json`: `acli jira workitem view <key> --fields=<the detail fields>,customfield_10020,customfield_10050 --json`:
  a parent, two links to another project's issues, a rich-text description (cut to its first six
  blocks), the sprint field and a user picker.
- `not_found.stderr`, `bad_query.stderr`, `field_not_allowed.stderr`: what `acli` printed on stderr
  for a key that does not exist, a query Jira could not parse, and a `--fields` entry that
  `workitem search` does not accept, each with exit status 1.
- `sprint_not_found.*`, `board_not_found.*`: a missing sprint and board, whose message is split
  between the two streams.
