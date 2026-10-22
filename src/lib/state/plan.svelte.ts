/**
 * Planning in the window: each opened issue's Plan tab, and every question card's thread.
 *
 * A store of its own, beside `startWork`, because two places act on the same records: the issue
 * workspace (the Plan tab, the Run section's questions) and the inbox (a question item). A question
 * answered, discussed or sent from one is the same question in the other, so both read its thread
 * from here.
 *
 * # Reading, never polling
 *
 * An issue's plan is read when its workspace opens, after each action here, and on `flow:changed`
 * for a run it shows: that is how a question, a reply and a plan arrive (ARCHITECTURE §8). Threads
 * are read the same way, by the run their question belongs to.
 */
import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import {
  errorMessage,
  type CommentDraft,
  type PlanView,
  type QuestionThread,
} from '../ipc/types';

class PlanState {
  /** The project the views below belong to. */
  projectId = $state<string | null>(null);
  /** Issues' planning, by key, as last read. `null` when the issue has no run. */
  views = $state<Record<string, PlanView | null>>({});
  /** Question threads, by question id. */
  threads = $state<Record<number, QuestionThread>>({});
  /** Send to Jira's open draft, by question id: the text being edited. */
  drafts = $state<Record<number, CommentDraft>>({});
  /** Keys, or `q<id>` for a question, with an action under way. */
  busy = $state<Record<string, boolean>>({});
  /** The last action's refusal, by the same keys. */
  errors = $state<Record<string, string>>({});

  async init(): Promise<() => void> {
    const off = await listen<{ flow: number }>('flow:changed', (event) => {
      const flow = event.payload.flow;
      const project = this.projectId;
      if (project) {
        // An issue read before it had a run has no flow to match: any run changing may be its
        // first, so it is read again too.
        for (const [key, view] of Object.entries(this.views)) {
          if (view === null || view.flow.id === flow) void this.load(project, key);
        }
      }
      for (const thread of Object.values(this.threads)) {
        if (thread.question.flow === flow) void this.thread(thread.question.id);
      }
    });
    return off;
  }

  private forProject(project: string): void {
    if (this.projectId === project) return;
    this.projectId = project;
    this.views = {};
  }

  /** Read `key`'s planning. */
  async load(project: string, key: string): Promise<void> {
    this.forProject(project);
    try {
      const view = await commands.issuePlan(project, key);
      if (this.projectId !== project) return;
      this.views = { ...this.views, [key]: view };
      if (view) {
        const threads = { ...this.threads };
        for (const thread of view.questions) threads[thread.question.id] = thread;
        this.threads = threads;
      }
    } catch (error) {
      if (this.projectId === project) this.fail(key, error);
    }
  }

  /** Read one question's thread, for a card the inbox shows. */
  async thread(question: number): Promise<void> {
    try {
      const thread = await commands.questionThread(question);
      this.threads = { ...this.threads, [question]: thread };
    } catch (error) {
      this.fail(`q${question}`, error);
    }
  }

  private fail(key: string, error: unknown): void {
    this.errors = { ...this.errors, [key]: errorMessage(error) };
  }

  private clear(key: string): void {
    this.errors = Object.fromEntries(
      Object.entries(this.errors).filter(([k]) => k !== key),
    );
  }

  private async act(key: string, action: () => Promise<void>, after: () => Promise<void>) {
    if (this.busy[key]) return false;
    this.busy = { ...this.busy, [key]: true };
    this.clear(key);
    let ok = true;
    try {
      await action();
    } catch (error) {
      ok = false;
      this.fail(key, error);
    } finally {
      this.busy = Object.fromEntries(Object.entries(this.busy).filter(([k]) => k !== key));
      await after();
    }
    return ok;
  }

  private issueAct(project: string, key: string, action: () => Promise<void>) {
    this.forProject(project);
    return this.act(key, action, () => this.load(project, key));
  }

  start(project: string, key: string, flow: number) {
    return this.issueAct(project, key, () => commands.startPlanning(flow));
  }

  resume(project: string, key: string, flow: number) {
    return this.issueAct(project, key, () => commands.resumePlanning(flow));
  }

  /** Check the issue: read it whole again, which is when a stale approval is found. */
  checkIssue(project: string, key: string, flow: number) {
    return this.issueAct(project, key, async () => {
      await commands.readPlannedIssue(flow);
    });
  }

  approve(
    project: string,
    key: string,
    flow: number,
    revision: number,
    issueRevision: string,
    skipDesign: boolean,
  ) {
    return this.issueAct(project, key, async () => {
      await commands.approvePlan(flow, revision, issueRevision, skipDesign);
    });
  }

  requestChanges(
    project: string,
    key: string,
    flow: number,
    revision: number,
    note: string,
  ) {
    return this.issueAct(project, key, async () => {
      await commands.requestPlanChanges(flow, revision, note);
    });
  }

  private questionAct(question: number, action: () => Promise<void>) {
    return this.act(`q${question}`, action, () => this.thread(question));
  }

  /** Answer from the issue workspace. The inbox answers through its own store; either counts. */
  answer(question: number, choice: number | null, text: string) {
    return this.questionAct(question, async () => {
      await commands.answerQuestion(question, choice, text);
    });
  }

  discuss(question: number, text: string) {
    return this.questionAct(question, () => commands.discussQuestion(question, text));
  }

  /** Send to Jira…: the draft, to edit. Nothing is recorded or sent. */
  draft(question: number) {
    return this.questionAct(question, async () => {
      const draft = await commands.questionCommentDraft(question);
      this.drafts = { ...this.drafts, [question]: draft };
    });
  }

  editDraft(question: number, body: string): void {
    const draft = this.drafts[question];
    if (draft) this.drafts = { ...this.drafts, [question]: { ...draft, body } };
  }

  closeDraft(question: number): void {
    this.drafts = Object.fromEntries(
      Object.entries(this.drafts).filter(([k]) => Number(k) !== question),
    );
  }

  /** The second click: post exactly the draft's text as it stands. */
  post(question: number) {
    const draft = this.drafts[question];
    if (!draft) return Promise.resolve(false);
    return this.questionAct(question, async () => {
      await commands.postQuestionComment(question, draft.body);
      this.closeDraft(question);
    });
  }

  retry(question: number, approval: number) {
    return this.questionAct(question, () => commands.retryQuestionComment(approval));
  }
}

export const plan = new PlanState();
