//! What a review role is told: its brief, which `flow_context` hands it, and the few turns the
//! engine sends.
//!
//! # The brief is facts, and the turn is short
//!
//! The first turn only says to call `flow_context`. Everything the role works from — its
//! objective, what it reads, its skills, and the exact schema its result is checked against — is
//! in the brief, which `flow_context` returns fenced as material rather than as the app speaking.
//! The objective and instructions come from your own config, but a later backlog item puts an
//! issue's text in the same place, and the fence is cheaper to keep than to add back.
//!
//! # Inputs that are not here yet
//!
//! A role may ask for the issue, a plan, a design, earlier findings or answers. A review of your own
//! work in this build has no issue, plan or design to give, and the brief says so by name rather
//! than leaving a role to wonder whether it missed them. Your rules and the repository's
//! instruction sources are given as R7 retrieves them; earlier findings are given when a role runs
//! again after the code changed, as the reports it made on the snapshot before. Answers are the
//! ones you gave to questions this role asked on an earlier attempt: a review role is not held for
//! an answer, so the next time it runs is the first time it can use one, and it gets them whether or
//! not it lists `answers`, because it asked.
//!
//! # A teammate's pull request
//!
//! A review of someone else's pull request says whose it is and what it merges into, because a
//! reviewer judges a contribution differently from your own unfinished work. When the round follows
//! the pull request to a new head, the earlier findings are the point of it: the role is told the
//! author pushed since, and asked to verify each one was addressed.

use std::fmt::Write as _;

use wtm_core::flow::planning::plan_text;
use wtm_core::flow::submission::{self, kind_name};
use wtm_core::flow::{
    CheckSpec, OutputKind, PlanContext, PlanRecord, PullRequest, Question, ReportedFinding,
    RoleInput, RoleProvider, RoleSpec, Snapshot, TrackerKind, Workspace,
};

/// What a brief is written from.
#[derive(Debug, Clone)]
pub struct BriefFacts<'a> {
    pub role: &'a str,
    pub spec: &'a RoleSpec,
    pub recipe: &'a str,
    pub worktree_path: &'a str,
    pub branch: Option<&'a str>,
    pub base: &'a str,
    pub snapshot: &'a Snapshot,
    /// The files the change touches, when git could list them all.
    pub changed: Option<&'a [String]>,
    /// For a fallback: what the role it stands in for got to.
    pub handover: Option<&'a str>,
    /// The repository's instructions and your rules, as `wtm_core::flow::knowledge::brief` wrote
    /// them, when the role takes either.
    pub knowledge: Option<&'a str>,
    /// The checks this role may ask the app to run, by id.
    pub checks: &'a [(String, CheckSpec)],
    /// What this role reported on an earlier snapshot, when it is run again after the code changed.
    /// Each with the id it is named by, which a report of it again puts in `repeats`.
    pub earlier: &'a [(String, ReportedFinding)],
    /// The pull request under review, for a teammate's, as the forge last described it.
    pub pull_request: Option<&'a PullRequest>,
    /// When this round follows the pull request to a new head: `(from, to)`.
    pub revision: Option<(&'a str, &'a str)>,
    /// Questions this role asked on an earlier attempt that you have answered since.
    pub answers: &'a [Question],
    /// For a runtime role: how it exercises the app, the pane it drives, and what was up.
    pub runtime: Option<&'a super::runtime::RuntimeSetup>,
}

/// What a runtime role is told about the running app it exercises.
fn runtime_section(out: &mut String, runtime: &super::runtime::RuntimeSetup) {
    let up: Vec<String> = runtime
        .services
        .iter()
        .map(|s| match &s.url {
            Some(url) => format!("`{}` ({url})", s.service),
            None => format!("`{}`", s.service),
        })
        .collect();
    let _ = writeln!(
        out,
        "\nYou are a runtime role: you exercise the running app, after the static roles have read \
         the code. Report what you saw the app do, not what the code suggests it would do."
    );
    if !up.is_empty() {
        let _ = writeln!(
            out,
            "- wtm asked each service just now, and these answered: {}.",
            up.join(", ")
        );
    }
    match (&runtime.pane, runtime.driver) {
        (Some(pane), _) => {
            let _ = writeln!(
                out,
                "- wtm opened a browser pane for you on the verified preview {}, in a private store \
                 with no cookies or sign-ins, which is thrown away when you hand in. Drive it with \
                 the `browser_*` tools; leave `browser` out, it is the only pane you have. It stays \
                 on that origin.",
                pane.url
            );
            let _ = writeln!(
                out,
                "- Every `browser_screenshot` is kept as evidence and its answer names it, like \
                 `s12`. A finding about something you saw on the page puts that name in its \
                 `evidence`, and a confirmed bug says in `verification` what you did to see it."
            );
            if let Some(opening) = &runtime.opening {
                let _ = writeln!(
                    out,
                    "- wtm took {} of the page as it first loaded, before you touched it.",
                    opening.id.label()
                );
            }
        }
        (None, wtm_core::flow::RuntimeDriver::Playwright) => {
            let _ = writeln!(
                out,
                "- This project runs its end-to-end tests with Playwright: run your checks with \
                 `flow_run_check` and report what they show. You have no browser pane."
            );
        }
        (None, wtm_core::flow::RuntimeDriver::Browser) => {}
    }
}

/// How many changed files a brief lists before it says how to list the rest.
const LISTED_FILES: usize = 60;

/// The name a role invokes `skill` by, in its provider's spelling.
///
/// A Claude run session gets its skills from the plugin `flow_session` writes, so they are listed
/// and invoked under the plugin's name. Codex lists a repository's skills under their own names.
#[must_use]
pub fn invocable(provider: RoleProvider, skill: &str) -> String {
    let skill = skill.trim_start_matches('/');
    match provider {
        RoleProvider::Claude => format!("{}:{skill}", wtm_agent::authority::RUN_SKILL_PLUGIN),
        RoleProvider::Codex => skill.to_owned(),
    }
}

/// The brief `flow_context` hands a review role.
#[must_use]
pub fn brief(facts: &BriefFacts<'_>) -> String {
    let spec = facts.spec;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Role `{}` in a review run of recipe `{}`.\n\nObjective: {}",
        facts.role,
        facts.recipe,
        spec.objective.trim()
    );
    if let Some(instructions) = spec.instructions.as_deref().map(str::trim)
        && !instructions.is_empty()
    {
        let _ = writeln!(out, "\n{instructions}");
    }

    if let Some(handover) = facts.handover {
        let _ = writeln!(out, "\nYou take over another session's work.\n{handover}");
    }

    let short = |sha: &str| sha.chars().take(10).collect::<String>();
    let snapshot = facts.snapshot;
    let _ = writeln!(out, "\nWhat you are reviewing:");
    if let Some(pr) = facts.pull_request {
        let author = pr.author.as_deref().map_or_else(
            || "an unknown author".to_owned(),
            |login| format!("@{login}"),
        );
        let head = match (&pr.head.repo, pr.cross_repository) {
            (Some(repo), true) => format!("{}:{}", repo.owner, pr.head.branch),
            _ => pr.head.branch.clone(),
        };
        let _ = writeln!(
            out,
            "- Pull request #{} on {}, \"{}\", by {author}: `{head}` into `{}`{}. It is a \
             teammate's change: review it as the person asked to approve it.",
            pr.number,
            pr.base.repo,
            pr.title.trim(),
            pr.base.branch,
            if pr.cross_repository {
                ", from a fork"
            } else {
                ""
            },
        );
    }
    let branch = facts
        .branch
        .map_or_else(String::new, |branch| format!(" (branch `{branch}`)"));
    match spec.workspace() {
        Workspace::Snapshot => {
            let _ = writeln!(
                out,
                "- Your working directory is a checkout of a snapshot of the worktree at `{}`{branch}, \
                 uncommitted and untracked work included. It is read-only, and the person keeps \
                 editing the real worktree while you read this one, so review this directory.",
                facts.worktree_path
            );
        }
        Workspace::Worktree => {
            let _ = writeln!(
                out,
                "- Your working directory is the worktree itself{branch}. It may change while you \
                 work; the run checks it again when you finish."
            );
        }
    }
    let _ = writeln!(
        out,
        "- The change is everything from the merge base with `{}` ({}) to the snapshot (tree {}). \
         `git diff {}` in your working directory shows all of it.",
        facts.base,
        short(&snapshot.base_sha),
        short(&snapshot.tree_sha),
        snapshot.base_sha
    );
    match facts.changed {
        Some([]) => {
            let _ = writeln!(out, "- No files differ from the merge base.");
        }
        Some(paths) => {
            let _ = writeln!(out, "- Files changed ({}):", paths.len());
            for path in paths.iter().take(LISTED_FILES) {
                let _ = writeln!(out, "  - {path}");
            }
            if paths.len() > LISTED_FILES {
                let _ = writeln!(
                    out,
                    "  - …and {} more; `git diff --stat {}` lists them.",
                    paths.len() - LISTED_FILES,
                    snapshot.base_sha
                );
            }
        }
        None => {}
    }

    if !spec.skills.is_empty() {
        let names: Vec<String> = spec
            .skills
            .iter()
            .map(|skill| format!("`{}`", invocable(spec.provider, skill)))
            .collect();
        let how = match spec.provider {
            RoleProvider::Claude => "Invoke them with the Skill tool",
            RoleProvider::Codex => "They are in your skills list; read and follow them",
        };
        let _ = writeln!(
            out,
            "\nSkills for this role: {}. {how}. Where a skill says how to report, report through \
             `flow_submit` instead, in the shape below.",
            names.join(", ")
        );
    }

    let mut missing: Vec<&str> = Vec::new();
    for name in spec.inputs.iter().filter_map(|input| match input {
        RoleInput::Issue => Some("the issue"),
        RoleInput::Plan => Some("an approved plan"),
        RoleInput::Design => Some("a design"),
        RoleInput::Rules | RoleInput::Instructions if facts.knowledge.is_none() => {
            Some("your review rules and the repository's instructions")
        }
        RoleInput::Findings if facts.earlier.is_empty() => Some("earlier findings"),
        RoleInput::Answers if facts.answers.is_empty() => Some("answers to questions"),
        RoleInput::Diff
        | RoleInput::Instructions
        | RoleInput::Rules
        | RoleInput::Findings
        | RoleInput::Answers => None,
    }) {
        // Rules and instructions arrive together, so a role asking for both is told once.
        if !missing.contains(&name) {
            missing.push(name);
        }
    }
    if let Some(knowledge) = facts.knowledge.filter(|text| !text.trim().is_empty()) {
        let _ = writeln!(out, "\n{}", knowledge.trim_end());
    }
    if let Some((from, to)) = facts.revision
        && !facts.earlier.is_empty()
    {
        let _ = writeln!(
            out,
            "\nThe author pushed new commits since you reviewed this pull request at {}; it is now \
             at {}. Verify that each finding you reported then was addressed: if one still holds, \
             report it again where it is now, with `repeats` set to its id; leave out any that \
             were addressed. Review what the new commits changed as well.",
            short(from),
            short(to)
        );
        for (id, earlier) in facts.earlier {
            let place = match (&earlier.path, earlier.line) {
                (Some(path), Some(line)) => format!(" ({path}:{line})"),
                (Some(path), None) => format!(" ({path})"),
                _ => String::new(),
            };
            let _ = writeln!(out, "- [{id}] {}{place}", earlier.title);
        }
    } else if spec.inputs.contains(&RoleInput::Findings) && !facts.earlier.is_empty() {
        let _ = writeln!(
            out,
            "\nYou reviewed an earlier snapshot of this change and reported the findings below. \
             The code has changed since. Check each one against the code as it is now: report it \
             again if it still holds, with `repeats` set to its id, and leave it out if it was \
             addressed."
        );
        for (id, earlier) in facts.earlier {
            let place = match (&earlier.path, earlier.line) {
                (Some(path), Some(line)) => format!(" ({path}:{line})"),
                (Some(path), None) => format!(" ({path})"),
                _ => String::new(),
            };
            let _ = writeln!(out, "- [{id}] {}{place}", earlier.title);
        }
    }
    if !facts.answers.is_empty() {
        let _ = writeln!(
            out,
            "\nOn an earlier attempt you asked the questions below, chose a reading and carried on. \
             The person has answered them since. Where an answer differs from the reading you \
             chose, review by the answer."
        );
        for question in facts.answers {
            let Some(answer) = &question.answer else {
                continue;
            };
            let picked = answer
                .choice
                .and_then(|choice| question.options.get(choice))
                .map(|option| format!("\"{option}\""));
            let said = Some(answer.text.trim())
                .filter(|text| !text.is_empty())
                .map(|text| format!("\"{text}\""));
            let reply = match (picked, said) {
                (Some(picked), Some(said)) => format!("{picked}, adding {said}"),
                (Some(only), None) | (None, Some(only)) => only,
                (None, None) => continue,
            };
            let _ = writeln!(out, "- {}\n  Answer: {reply}", question.text.trim());
        }
    }
    if !facts.checks.is_empty() {
        let _ = writeln!(
            out,
            "\nChecks you may ask the app to run with `flow_run_check`, by name. The app runs \
             them outside your sandbox, in your working directory, and records the result as \
             evidence; cite a check's result only from what that call returned, never from what \
             you expect it to do."
        );
        for (id, check) in facts.checks {
            let label = check
                .label
                .as_deref()
                .map_or_else(String::new, |label| format!(" ({label})"));
            let needs = if check.needs.is_empty() {
                String::new()
            } else {
                format!(
                    " — needs {} answering; if one is not, it is recorded blocked and not run",
                    check.needs.join(", ")
                )
            };
            let _ = writeln!(out, "- `{id}`{label}: `{}`{needs}", check.run.join(" "));
        }
    }
    if let Some(runtime) = facts.runtime {
        runtime_section(&mut out, runtime);
    }
    if !missing.is_empty() {
        let _ = writeln!(
            out,
            "\nThis role asks for {}, and this review has none to give. Work from the change itself.",
            missing.join(", ")
        );
    }

    let kind = spec.output;
    let _ = writeln!(
        out,
        "\nHand in your result by calling `flow_submit` with kind `{}` and a payload matching this \
         JSON Schema. It is checked; a payload that does not match comes back with every problem \
         listed, to fix and submit again. Your result is what you submit, not what you write in \
         chat.\n{}",
        kind_name(kind),
        submission::schema(kind)
    );
    if kind == OutputKind::Findings {
        let _ = writeln!(
            out,
            "\nReport each problem once. A confirmed bug is one you demonstrated, with the \
             `verification` that showed it; anything you suspect but did not confirm is a \
             suggestion. A conflict between two sources of guidance is its own finding: do not \
             pick a side. An empty `findings` list is a valid result."
        );
    }
    let _ = write!(
        out,
        "\nWhen you have submitted, end your turn. Do not try to change files, commit or push: \
         this session cannot, and nothing you do here is published."
    );
    out
}

/// The brief `flow_context` hands a writer fixing findings of a teammate's pull request.
///
/// Facts as a review's brief is, and the same fence: the findings' text is a reviewer's words about
/// someone else's code. What differs is what the role may do — change the files those findings
/// name, and nothing else — and that its commits are the app's, through `flow_checkpoint`.
#[must_use]
pub fn fix_brief(facts: &BriefFacts<'_>, fix: &super::fix::FixLive) -> String {
    let spec = facts.spec;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Role `{}`: you fix findings from a review, as a writer.\n\nObjective: {}",
        facts.role,
        spec.objective.trim()
    );
    if let Some(instructions) = spec.instructions.as_deref().map(str::trim)
        && !instructions.is_empty()
    {
        let _ = writeln!(out, "\n{instructions}");
    }
    let _ = writeln!(out, "\nWhere you work:");
    if let Some(pr) = facts.pull_request {
        let _ = writeln!(
            out,
            "- Pull request #{} on {}, \"{}\": `{}` into `{}`. It is a teammate's change; you are \
             fixing what a review of it found, for the person who reviewed it.",
            pr.number,
            pr.base.repo,
            pr.title.trim(),
            pr.head.branch,
            pr.base.branch
        );
    }
    let _ = writeln!(
        out,
        "- Your working directory is the review worktree at `{}`, at the pull request's head \
         `{}` or a fix made on top of it. It may already hold edits from an earlier attempt of \
         yours: look at `git status` and carry on from them.",
        facts.worktree_path,
        facts.snapshot.head_sha.chars().take(10).collect::<String>()
    );
    let _ = writeln!(out, "\nFix exactly these findings:");
    for record in &fix.findings {
        let finding = &record.finding;
        let place = finding.location.as_ref().map_or_else(
            || "the whole change".to_owned(),
            |at| format!("{}:{}-{}", at.path, at.start_line, at.end_line),
        );
        let _ = writeln!(
            out,
            "- [{}] {} ({place}, {:?}): {}",
            finding.id,
            finding.title.trim(),
            finding.severity,
            finding.rationale.trim()
        );
    }
    let scope: Vec<String> = fix.scope.paths.iter().map(|p| format!("`{p}`")).collect();
    let _ = writeln!(
        out,
        "\nYou may change only these files: {}. A change to any other file is refused when it is \
         committed, and the person is asked whether to let it in; until they say yes, it stays out \
         of the fix.",
        scope.join(", ")
    );
    let _ = writeln!(
        out,
        "\nYou cannot commit or push. When a finding is fixed, call `flow_checkpoint` with a commit \
         message saying what you fixed and why; the app makes the commit, as the person, after \
         checking the files. Then hand in a handoff with `flow_submit`, kind `handoff`, matching \
         this JSON Schema:\n{}",
        submission::schema(OutputKind::Handoff)
    );
    let _ = write!(
        out,
        "\nDo not push, open pull requests or post anything: where this fix goes is the person's \
         decision, made after reading its diff."
    );
    let fenced = facts.knowledge.map(str::trim).filter(|k| !k.is_empty());
    if let Some(knowledge) = fenced {
        let _ = write!(
            out,
            "\n\nThe repository's guidance and your rules:\n{knowledge}"
        );
    }
    out
}

/// The first turn a fix's writer session is sent.
#[must_use]
pub fn fix_opening_turn(role: &str) -> String {
    format!(
        "Start the `{role}` fix: call `flow_context` for your brief, fix the findings it lists in \
         the files it allows, commit with `flow_checkpoint`, and hand in a handoff with \
         `flow_submit`."
    )
}

/// What a planner's brief is written from.
#[derive(Debug, Clone)]
pub struct PlannerFacts<'a> {
    pub role: &'a str,
    pub spec: &'a RoleSpec,
    pub key: &'a str,
    pub tracker: TrackerKind,
    pub worktree_path: &'a str,
    pub branch: Option<&'a str>,
    /// What the planner was given, as the context drawer lists it.
    pub context: Option<&'a PlanContext>,
    /// The repository's instructions and your rules, as `wtm_core::flow::knowledge::brief` wrote
    /// them, when the role takes either.
    pub knowledge: Option<&'a str>,
    /// Questions it asked that you have answered.
    pub answers: &'a [Question],
    /// The newest plan it handed in, when it is revising one.
    pub latest: Option<&'a PlanRecord>,
}

/// The brief `flow_context` hands a planner.
///
/// The issue's text is in here, inside `flow_context`'s fence like everything else, because an
/// issue is text anybody with access to the tracker wrote. What the planner may do, when to ask and
/// what to hand in are the app's words, before and after it.
#[must_use]
pub fn planner_brief(facts: &PlannerFacts<'_>) -> String {
    let spec = facts.spec;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Role `{}`: you investigate {} {} and plan its implementation. You do not implement \
         it.\n\nObjective: {}",
        facts.role,
        facts.tracker.as_str(),
        facts.key,
        spec.objective.trim()
    );
    if let Some(instructions) = spec.instructions.as_deref().map(str::trim)
        && !instructions.is_empty()
    {
        let _ = writeln!(out, "\n{instructions}");
    }

    if let Some(context) = facts.context {
        planner_issue(&mut out, context);
    }

    let branch = facts
        .branch
        .map_or_else(String::new, |branch| format!(" (branch `{branch}`)"));
    let _ = writeln!(
        out,
        "\nYour working directory is the issue's worktree{branch}, at `{}`. Read the code there; \
         your sandbox refuses every write, in the worktree too, because nothing is written before \
         a plan is approved.",
        facts.worktree_path
    );

    if !spec.skills.is_empty() {
        let names: Vec<String> = spec
            .skills
            .iter()
            .map(|skill| format!("`{}`", invocable(spec.provider, skill)))
            .collect();
        let how = match spec.provider {
            RoleProvider::Claude => "Invoke them with the Skill tool",
            RoleProvider::Codex => "They are in your skills list; read and follow them",
        };
        let _ = writeln!(
            out,
            "\nSkills for this role: {}. {how}. Where a skill says how to report, hand in a plan \
             through `flow_submit` instead.",
            names.join(", ")
        );
    }
    if let Some(knowledge) = facts.knowledge.filter(|text| !text.trim().is_empty()) {
        let _ = writeln!(out, "\n{}", knowledge.trim_end());
        let _ = writeln!(
            out,
            "\nFor a plan, a disagreement between the project's instructions and a rule is a \
             question for the person, or a risk the plan names. Never pick a side silently."
        );
    }

    if !facts.answers.is_empty() {
        let _ = writeln!(out, "\nQuestions you asked, and the person's answers:");
        for question in facts.answers {
            let Some(answer) = &question.answer else {
                continue;
            };
            let _ = writeln!(
                out,
                "- {}\n  Answer: {}",
                question.text.trim(),
                super::planner::answer_words(question, answer)
            );
        }
    }
    if let Some(latest) = facts.latest {
        let _ = writeln!(
            out,
            "\nYour newest plan, revision {}:\n{}",
            latest.revision,
            plan_text(&latest.plan)
        );
    }

    let _ = writeln!(
        out,
        "\nAsk with `flow_ask` only when the answer changes the work's behaviour, its scope or its \
         architecture, and you cannot settle it from the code, the issue or the project's \
         instructions. Anything else, decide reasonably and say what you decided in the plan. A \
         question gives `why` it matters, `refs` (file:line, or a quoted line of the issue), your \
         `recommendation`, what it `unblocks`, and `options` when there are clear choices. When \
         you have asked, end your turn: wtm ends it for you, and the answer arrives as your next \
         message in this same conversation, possibly hours later."
    );
    let _ = writeln!(
        out,
        "\nHand in the plan by calling `flow_submit` with kind `plan` and a payload matching this \
         JSON Schema. It is checked; a payload that does not match comes back with every problem \
         listed, to fix and submit again. `areas` are the files and directories the work expects \
         to touch: a writer is held to them later, so name every place, as globs where that is \
         honest. Set `designNeeded` when the work changes a user interface substantially or a \
         person should see it before it is built.\n{}",
        submission::schema(OutputKind::Plan)
    );
    let _ = write!(
        out,
        "\nWhen you have submitted, end your turn. Do not try to change files, commit or push: \
         this session cannot, and nothing is implemented until the plan is approved."
    );
    out
}

/// The issue as the planner's brief gives it: the context drawer's record, in words.
fn planner_issue(out: &mut String, context: &PlanContext) {
    let issue = &context.issue;
    let _ = writeln!(
        out,
        "\nThe issue, as the tracker had it when this run read it (status: {}):",
        context.status
    );
    let _ = writeln!(out, "- Summary: {}", issue.summary.trim());
    if let Some(kind) = &issue.kind {
        let _ = writeln!(out, "- Type: {kind}");
    }
    match issue.description.as_deref().map(str::trim) {
        Some(description) if !description.is_empty() => {
            let _ = writeln!(out, "- Description:\n{description}");
        }
        _ => {
            let _ = writeln!(out, "- Description: none");
        }
    }
    for field in &issue.fields {
        let _ = writeln!(
            out,
            "- {}: {}",
            field.name,
            field.value.as_deref().unwrap_or("(empty)")
        );
    }
    if let Some(sprint) = &context.sprint {
        let goal = context
            .sprint_goal
            .as_deref()
            .map_or_else(String::new, |goal| format!(", whose goal is: {goal}"));
        let _ = writeln!(out, "- Sprint: {sprint}{goal}");
    }
    if !context.dependencies.is_empty() {
        let _ = writeln!(
            out,
            "\nRelated issues, as read-only context. They are other work; plan this issue, and \
             name a dependency in the plan's risks where it matters:"
        );
        for dependency in &context.dependencies {
            let summary = dependency
                .summary
                .as_deref()
                .map_or_else(String::new, |s| format!(" · {s}"));
            let status = dependency
                .status
                .as_deref()
                .map_or_else(String::new, |s| format!(" ({s})"));
            let _ = writeln!(
                out,
                "- {} {}{summary}{status}",
                dependency.relation, dependency.key
            );
        }
    }
}

/// The first turn a planner is sent.
#[must_use]
pub fn planner_opening_turn(key: &str) -> String {
    format!(
        "Start planning {key}: call `flow_context` for your brief, investigate the code, ask with \
         `flow_ask` only if an answer would change behaviour, scope or architecture, and hand in \
         the plan with `flow_submit`."
    )
}

/// The turn that resumes a planner with the answers to its questions.
#[must_use]
pub fn planner_answers_turn(answered: &[Question], resumed: bool) -> String {
    let mut out = String::from(if resumed {
        "The person answered your question"
    } else {
        "This is a new session: the conversation you asked in could not be resumed. The person \
         answered the question you asked"
    });
    if answered.len() > 1 {
        out.push('s');
    }
    out.push_str(".\n");
    for question in answered {
        if let Some(answer) = &question.answer {
            let _ = writeln!(
                out,
                "- {}\n  Answer: {}",
                question.text.trim(),
                super::planner::answer_words(question, answer)
            );
        }
    }
    out.push_str(
        "Continue planning with the answer. Ask again only if something new changes behaviour, \
         scope or architecture, and hand in the plan with `flow_submit` when it is ready.",
    );
    if !resumed {
        out.push_str(" Call `flow_context` first for your brief.");
    }
    out
}

/// The turn that asks a planner about one of its questions before it is answered.
#[must_use]
pub fn planner_discuss_turn(question: &Question, message: &str) -> String {
    format!(
        "The person has not answered your question yet (\"{}\"). Before they do, they ask:\n\n{}\n\n\
         Reply to them in plain words in this turn, briefly, as one message. Do not call \
         `flow_submit` or `flow_ask` and do not continue planning: the question stays open until \
         they answer it, and your reply is shown to them beside it.",
        question.text.trim(),
        message.trim()
    )
}

/// The turn that asks a planner for changes to a plan revision.
#[must_use]
pub fn planner_changes_turn(revision: u32, note: &str, resumed: bool) -> String {
    let start = if resumed {
        ""
    } else {
        "This is a new session: the conversation could not be resumed. Call `flow_context` \
         first; it holds your brief and your newest plan. "
    };
    format!(
        "{start}The person asked for changes to plan revision {revision}:\n\n{}\n\nRevise the \
         plan and hand the whole of it in again with `flow_submit`. Ask with `flow_ask` only if \
         the request leaves something that changes behaviour, scope or architecture unclear.",
        note.trim()
    )
}

/// The turn that carries a planner on after an interruption, a failure or a limit.
#[must_use]
pub fn planner_resume_turn(key: &str, resumed: bool) -> String {
    if resumed {
        format!(
            "Your work on {key} was cut off. Carry on from where you were, and hand in the plan \
             with `flow_submit`."
        )
    } else {
        planner_opening_turn(key)
    }
}

/// The first turn a role session is sent.
#[must_use]
pub fn opening_turn(role: &str) -> String {
    format!(
        "Start the `{role}` review: call `flow_context` for your brief, do the review it describes, \
         and hand in your result with `flow_submit`."
    )
}

/// The turn sent to a role that ended its turn without handing in a result.
#[must_use]
pub fn nudge(kind: OutputKind) -> String {
    format!(
        "Your turn ended without a result. The run reads your result only from `flow_submit`, with \
         kind `{}`. Call it now with what you found — an empty list is a valid result if you found \
         nothing — and then end your turn.",
        kind_name(kind)
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wtm_core::flow::BlobRef;

    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            head_sha: "1".repeat(40),
            tree_sha: "2".repeat(40),
            base_sha: "3".repeat(40),
            patch: BlobRef {
                path: "p".to_owned(),
                sha256: "s".to_owned(),
                bytes: 1,
            },
        }
    }

    fn role(value: serde_json::Value) -> RoleSpec {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_claude_role_is_told_its_skills_by_the_name_its_session_lists_them_under() {
        let spec = role(json!({
            "objective": "Check conventions",
            "provider": "claude",
            "skills": ["/team-conventions"],
            "inputs": ["diff", "rules"],
        }));
        let changed = ["src/a.rs".to_owned()];
        let text = brief(&BriefFacts {
            role: "conventions",
            spec: &spec,
            recipe: "standard",
            worktree_path: "/w/acme-wt",
            branch: Some("fix/x"),
            base: "origin/main",
            snapshot: &snapshot(),
            changed: Some(&changed),
            handover: None,
            knowledge: None,
            checks: &[],
            earlier: &[],
            pull_request: None,
            revision: None,
            answers: &[],
            runtime: None,
        });
        assert!(text.contains("`wtm:team-conventions`"), "{text}");
        assert!(text.contains("Skill tool"));
        assert!(
            text.contains("your review rules"),
            "a missing input is named"
        );
        assert!(text.contains(&format!("git diff {}", "3".repeat(40))));
        assert!(text.contains("  - src/a.rs"));
        assert!(
            text.contains(r#""confirmed_bug""#),
            "the schema itself is in the brief"
        );
    }

    #[test]
    fn a_role_running_again_is_given_the_answers_to_what_it_asked_before() {
        use wtm_core::flow::{Answer, FlowId, QuestionId};

        let spec = role(json!({
            "objective": "Find bugs",
            "provider": "codex",
            "inputs": ["diff", "answers"],
        }));
        let answered = Question {
            id: QuestionId(1),
            flow: FlowId(1),
            step: None,
            role: Some("correctness".to_owned()),
            text: "Is an empty export an error?".to_owned(),
            why: String::new(),
            unblocks: String::new(),
            blocks: None,
            context: Vec::new(),
            recommendation: None,
            options: vec!["An error".to_owned(), "Valid, header only".to_owned()],
            asked_at: 1,
            answer: Some(Answer {
                choice: Some(1),
                text: "and log it".to_owned(),
                at: 2,
            }),
        };
        let snapshot = snapshot();
        let facts = |answers| BriefFacts {
            role: "correctness",
            spec: &spec,
            recipe: "standard",
            worktree_path: "/w/acme-wt",
            branch: None,
            base: "origin/main",
            snapshot: &snapshot,
            changed: None,
            handover: None,
            knowledge: None,
            checks: &[],
            earlier: &[],
            pull_request: None,
            revision: None,
            answers,
            runtime: None,
        };

        let text = brief(&facts(std::slice::from_ref(&answered)));
        assert!(text.contains("- Is an empty export an error?"), "{text}");
        assert!(
            text.contains(r#"Answer: "Valid, header only", adding "and log it""#),
            "{text}"
        );
        assert!(
            !text.contains("answers to questions"),
            "an input that is given is not called missing"
        );

        let without = brief(&facts(&[]));
        assert!(without.contains("answers to questions"), "{without}");
    }

    #[test]
    fn a_codex_skill_keeps_its_own_name() {
        assert_eq!(invocable(RoleProvider::Codex, "/review"), "review");
        assert_eq!(invocable(RoleProvider::Claude, "review"), "wtm:review");
    }
}
