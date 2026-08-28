---
name: find-simplifications
description: 'Use when working in this repo to find non-obvious simplification candidates, write proposed notes or inline TODO/FIXME/XXX notes, audit or coalesce superseded notes, or fold worthwhile simplification ideas from another branch; especially for dead, duplicated, speculative, over-built, added-then-removed, or hand-rolled-where-a-dependency-exists surfaces.'
---

# Finding Simplifications

This skill helps turn a broad "find things to simplify" request into evidence-backed notes that remove or collapse existing surface area. It is guidance, not a checklist: follow the code, keep judgment active, and prefer a few well-proven candidates over a pile of thin guesses.

## Start With Repo Context

- Read the root `AGENTS.md` files and the [documentation standard](../../../docs/AGENTS.md).
- Use the note tree and its [rules](../../../docs/notes/AGENTS.md) to understand intentional architecture; a simplification that collapses a decision recorded in an implemented note needs evidence that beats the recorded rationale, not just a smell.
- Treat recorded seams and deliberately dual mechanisms as intentional by default. Removing an unused method or hook inside a protected seam can still be valid if it does not collapse the protected design.

## What Counts As A Strong Candidate

A strong simplification removes, folds, or demotes something real and has clear evidence that the current design costs more than it buys:

- A public method, endpoint, config knob, header, helper, crate, or test artifact has no production consumer.
- Tests or docs are the only consumers, and the behavior they pin is not load-bearing.
- Two representations mirror the same fact.
- A seam has methods every implementation must support but no consumer uses.
- A separate crate or module exists only for test/demo/support code and adds build or dependency overhead.
- A feature implements speculative product generality with no product owner.
- An invariant, rollback path, set of expected outputs, or special-case test exists only to protect an unused API.
- Hand-rolled code reimplements what a well-maintained external crate or the standard library already provides, and the swap would delete the implementation plus its dedicated tests.
- The simplified behavior may differ slightly, but the new behavior is still reasonable and easier to explain.

Thin candidates are usually not enough for a note: deleting one typo, removing an intentionally documented backend/adapter, or flagging "this looks complex" without call-site proof.

## Survey Broadly

Use parallel subagents when the user asks for breadth or many candidates. Give each agent a domain and require evidence, not guesses. Useful domains:

- Flow scheduling: condition evaluation, pool construction, weighted selection, penalties, plugin hooks.
- Sessions and challenges: session lifecycle, chunk tracking, challenge generation and verification, legacy-client paths.
- Storage backends: per-backend URL signing, capability gates, health checks, URL rewriting.
- Cache and version files: cache keys and metadata, census and majority verdicts, version providers.
- Observability: metrics registration, session log schema, the log analysis command.
- Tests/fixtures/scripts: redundant fixtures, static inventories, support scripts.

If subagents are unavailable, simulate the same breadth yourself. Do not let the first good candidate stop the survey.

Start with the largest production-code deltas. A broad simplification audit that stops after obvious unused symbols can miss the files where duplicated lifecycle or defensive machinery carries most of the cost.

## Audit Trust And Lifecycle Boundaries

For every defensive copy, freeze, validator, and callback capture, name where the value came from and who owns it next. Same-process typed service/plugin calls ordinarily borrow readonly values; parsers, config loaders, queues, model/tool JSON, durable files, workers, processes, and wire decoders own or validate their data. Tests built around hostile getters, fake typed objects, callback replacement, or mutation after a same-process handoff are evidence of a potentially speculative contract, not automatic justification for keeping it.

For complex asynchronous code, draw the ownership graph and map each sentinel, readiness promise, cancellation path, disposer, and state flag to a distinct owner or transition. When several mechanisms mirror the same liveness or settlement fact, propose one transaction or lifecycle controller instead. Preserve separate machinery where it protects synchronous publication and rollback, callback containment, first-terminal-outcome arbitration, worker/process ownership, or dispose-to-quiescence.

## Hand-Rolled Code Versus A Dependency

Introducing a dependency is a valid simplification move, not a policy exception. When surveying, ask of protocol parsers, framers, retry/backoff loops, glob matchers, and similar infrastructure: does a well-maintained crate or the standard library already do this?

Prove a dependency-swap candidate like any other, plus:

- Read the hand-rolled implementation and name the exact surface the crate covers; residual semantics the crate does not cover count against the swap and stay in the note.
- Check the crate's health honestly (maintenance, adoption, transitive footprint) and prefer the standard library when it suffices.
- Check the note tree first: recorded seams are settled — a swap that collapses one needs to beat the recorded rationale, not just cite the policy.
- Weigh net deletion: implementation plus dedicated tests plus docs, minus the glue that remains. A wrapper that relocates the same complexity is not a win.

## Prove Or Reject Each Candidate

For every symbol or behavior, classify consumers before writing:

- Production corpus: crate `src/` trees, runtime scripts, and config paths.
- Non-production corpus: tests, README/docs, notes, fixtures, generated expected outputs, and comments.
- Ambiguous corpus: examples and scripts that may be product smoke paths. Inspect usage before classifying.

Use `rg` first. Good searches include the exact symbol, header name, config key, env var, method name, and any wire strings. Then read the call sites. Compiler dead-code warnings can help, but they are not a substitute for understanding public interfaces, dynamic dispatch, tests, and docs.

Reject or downgrade a candidate when:

- A production caller exists and the simplification would be a feature decision rather than a cleanup.
- The API is explicitly justified by an implemented note or a hard-won defensive pattern, and the new evidence does not beat that reason.
- The removal would force unrelated churn without actually reducing the public API or required behavior.
- The idea is correct but tiny. Add a targeted TODO/FIXME/XXX instead.

## Coalesce Superseded Notes

Audit the note tree when the user asks to reduce or coalesce it, or when the simplification being implemented makes an owning note obsolete. Do not expand every code-simplification survey into a repository-wide note audit.

Use [`archive-notes`](../archive-notes/SKILL.md) for retention judgment and archive mechanics. Low-future-value implemented notes move frozen to `archived/`; proposed notes are never archived; rejected notes that no longer prevent a tempting mistake are deleted. Do not edit an archived note while simplifying current prose or code.

Follow the deletion rule in the [note rules](../../../docs/notes/AGENTS.md); do not duplicate or weaken it here. For each candidate chain:

1. Identify the current owner from shipped code, configuration, docs, newer notes, and inbound links; dates and titles are discovery hints, not proof.
2. Classify the old note as fully or partially superseded. Any surviving behavior, current contract, durable format, compatibility obligation, or independently current rejected alternative makes it partial. Rationale that can be transferred to the current owner does not by itself make supersession partial.
3. For full supersession, move every unique rationale, alternative, consequence, shipped verification evidence, and named coverage gap into the current owner. An inventory that only describes deleted implementation mechanics is not one of those decision facts.
4. Repair every inbound link, then delete the note.
5. Search exact filenames, symbols, config keys, header names, and wire strings after the edit. Keep partial supersessions cross-linked and current.

An added-then-removed feature is a common full-supersession case. Let the removal note own the history only when the feature is absent from production code, configuration, schemas, durable or wire formats, migration, and compatibility behavior; no current documentation presents it as available; and no test exercises it as supported behavior. Removal rationale and tests that enforce absence may remain. Preserve why the feature originally existed, why that motivation no longer justified it, alternatives to full removal, the capability given up, conditions for reintroduction, and evidence that removal is complete. Old tests and implementation mechanics that verified only the deleted behavior are not current verification evidence.

Reject consolidation when the removal is only one transport, default, implementation, or presentation of a feature; when persisted data or compatibility handling survives; or when the removal note does not yet carry enough rationale to prevent accidental reintroduction. A current negative design decision may legitimately need its own note even though the removed implementation is gone.

## Write The Note

Create one file per durable proposal under `docs/notes/proposed/yyyy-mm-dd-<slug>.md`, following the [note rules](../../../docs/notes/AGENTS.md) for header lines and sections. Keep prose paragraphs on one physical line and use relative Markdown links.

Section guidance for a simplification proposal:

- `## Problem`: name the current API, cite the relevant files, and state the consumer evidence. Separate production callers from tests/docs.
- `## Proposal`: say exactly what to remove, fold, demote, or rehome. Include tests, docs, and fixture cleanup when relevant.
- `## Alternatives considered`: make the strongest counterargument for keeping it legible.
- `## Acceptance criteria`: observable end state and gates.
- `## Risks`: public API changes, behavior changes, future product wants, and why the tradeoff is still reasonable.

Be concrete enough that an implementing change can follow the trail. Avoid vague "simplify this module" notes. When a proposal overlaps an existing note, consolidate the useful details into the existing one rather than creating a duplicate.

## Inline TODO Notes

Use inline TODO/FIXME/XXX only for small, local cleanups that are clearly useful but not durable design decisions. Keep them short and actionable:

- Name the smell with a stable tag, e.g. `TODO(double-default)` or `XXX(unused-default)`.
- Explain why it is safe to revisit and what action would simplify it.
- Do not add TODOs for speculative complaints or for behavior that needs a note-level decision.

## When Folding Another Branch

Diff the sibling branch against the main branch, not against the current working branch, so you see its independent contribution. For each item:

- Port non-overlapping notes or TODOs that meet the quality bar.
- Consolidate overlapping material into the existing note that owns the topic.
- Do not port duplicate or lower-confidence proposals just to preserve the count.

## Validation And Reporting

For docs-only note work, run at least `git diff --check` and verify every relative link resolves. For code changes, run `cargo fmt --all -- --check`, `cargo clippy`, and the relevant tests.

When reporting results, summarize:

- How many notes and inline notes were added, consolidated, retained as partial supersessions, or deleted.
- The main areas surveyed.
- What was intentionally excluded.
- Which checks passed.

For each consolidation group, name the old and current owners, state the evidence for full supersession, and explain why deletion is safe. If an added-then-removed scan finds no qualifying note, report that result and the representative partial cases retained.
