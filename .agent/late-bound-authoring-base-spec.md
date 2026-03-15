# Patch SPEC.md for late-bound implicit authoring bases

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agent/PLANS.md`.

## Purpose / Big Picture

After this change, the spec will no longer require an implicitly seeded revision to begin authoring from whatever `target_ref` happened to point at when the revision was created. Instead, a revision with no explicit `seed_commit_oid` will bind its authoring base at the first mutating authoring dispatch, which creates the initial authoring workspace using the then-current `target_ref` head. This preserves review and convergence stability once work starts, while avoiding stale implicit bases for queued work.

## Progress

- [x] 2026-03-14 22:31Z Confirmed that `SPEC.md` currently freezes implicit `seed_commit_oid` values at revision creation and threads that eager value through authoring, review, and convergence.
- [x] 2026-03-14 22:33Z Chose the spec shape: keep `seed_target_commit_oid` as the eager audit snapshot, make `seed_commit_oid` an explicit nullable override, and define the authoring workspace `base_commit_oid` as the canonical bound authoring base for implicit revisions.
- [x] 2026-03-14 22:37Z Patched `SPEC.md` sections covering revision semantics, authoring workspace lifecycle, mutating job protocol, review subjects, convergence replay, revision-creation defaults, database/conformance notes, and reference algorithms.
- [x] 2026-03-14 22:45Z Tightened the spec after review: first mutating authoring dispatch now binds the implicit base before job creation, and `investigate_item` is explicitly allowed before authoring with an ephemeral target-head diff subject.
- [x] 2026-03-14 22:51Z Closed follow-up review gaps: dispatch-time binding now explicitly includes workspace creation plus initial job input seeding, and pre-authoring investigation subjects now have an explicit durability requirement in cleanup and reconciliation rules.
- [x] 2026-03-14 22:56Z Clarified the remaining mechanics: pre-authoring investigation now explicitly creates or adopts a durable local commit reference, and the dispatch algorithm now creates the first implicit authoring job with the resolved initial head instead of describing an impossible pre-job write.
- [x] 2026-03-14 23:05Z Extended `formal/AuthoringBaseBinding.tla` to cover pre-authoring `investigate_item`, including durable anchor retention, triage, and cleanup, and re-ran TLC successfully.

## Surprises & Discoveries

- Observation: the current spec already has a durable place to store a late-bound authoring base without inventing a new revision field.
  Evidence: `Workspace.base_commit_oid` already exists and is optional for authoring workspaces in `SPEC.md`, so the patch can promote it to required-once-provisioned and use it as the canonical bound base for implicit revisions.

- Observation: superseding revision defaults need separate treatment from initial item creation.
  Evidence: `POST /items` can safely leave `seed_commit_oid` implicit, but `revise`, `reopen`, and `approval/reject` should still carry forward the prior current authoring head when work already exists.

- Observation: allowing pre-authoring `investigate_item` required a distinct rule for its diff subject.
  Evidence: otherwise the general auxiliary dispatch rule contradicted the new review-subject rule for implicitly seeded revisions with no authoring workspace yet.

- Observation: once pre-authoring investigation became legal, the existing finding-anchor rules no longer guaranteed reachability for that new subject type.
  Evidence: cleanup and startup reconciliation originally mentioned authoring anchors, integration anchors, and generic durable refs, but not the specific requirement to retain the ephemeral investigation snapshot while its findings remain untriaged.

- Observation: the first dispatch pseudocode still implied writing a job field before the job row existed.
  Evidence: the algorithm said to "record" the new job's `input_head_commit_oid` before `create_job_for_step(step)`, which is sequencing nonsense unless job creation takes that value as an input.

- Observation: the original focused TLA+ model covered first-authoring binding and convergence drift but not the newly allowed pre-authoring `investigate_item` branch.
  Evidence: `formal/AuthoringBaseBinding.tla` initially had no variables or transitions for the durable pre-authoring investigation anchor or its triage lifecycle.

## Decision Log

- Decision: store the implicit late-bound authoring base on the authoring workspace instead of adding a new mutable revision field.
  Rationale: the revision model stays immutable, while the already-existing authoring workspace lifecycle is the natural place where first provisioning occurs and where `base_commit_oid` can be bound exactly once.
  Date/Author: 2026-03-14 / Codex

- Decision: keep `seed_target_commit_oid` eager and frozen at revision creation.
  Rationale: it remains useful as an audit snapshot and promotion default even though implicit authoring now binds later.
  Date/Author: 2026-03-14 / Codex

- Decision: preserve carried-forward current work for superseding revisions by default.
  Rationale: the late-binding rule is meant to avoid stale untouched work, not to discard in-progress candidate work when a revision is superseded.
  Date/Author: 2026-03-14 / Codex

- Decision: permit `investigate_item` before authoring-base binding, but make it use an ephemeral `target_ref` snapshot rather than binding the revision.
  Rationale: this preserves the existing “investigate from any open idle item” behavior without letting a report-only step silently choose the revision’s long-lived authoring base.
  Date/Author: 2026-03-14 / Codex

- Decision: require explicit retention of the durable commit reference used for any pre-authoring investigation subject until its findings are triaged.
  Rationale: without this, the new permissible investigation path would create candidate findings whose source subject could become unreachable before triage or promotion decisions.
  Date/Author: 2026-03-14 / Codex

- Decision: make first implicit authoring dispatch pass the resolved head into job creation instead of describing a pre-creation write to the job row.
  Rationale: this preserves the intended semantics while keeping the reference algorithm operationally coherent.
  Date/Author: 2026-03-14 / Codex

- Decision: model only the pre-authoring investigation path in TLA+, not the full report-only step matrix.
  Rationale: the formal risk introduced by the spec change is specifically the creation, retention, and cleanup of the durable anchor before authoring begins. Modeling that branch keeps the state space small while covering the new safety rule.
  Date/Author: 2026-03-14 / Codex

## Outcomes & Retrospective

`SPEC.md` now distinguishes explicit source baselines from implicit authoring-base binding. The patched spec keeps candidate review and convergence stable once a revision starts, allows untouched queued revisions to begin on the latest project head at first mutating authoring dispatch, preserves pre-authoring `investigate_item` by giving it an ephemeral target-head diff subject, and explicitly retains that subject while its findings remain untriaged.

## Context and Orientation

`SPEC.md` is the normative workflow contract for this repository. The relevant sections are `4.5 Workspace`, `4.7 ItemRevision`, `7.4 Mutating Job Protocol`, `7.6 Review Subjects`, `7.7 Convergence Lifecycle`, `8.4 Item Command Semantics`, `11.2 Database Enforcement`, `12.4 Prepare Convergence`, and `14. Conformance Matrix`. In this patch, a "bound authoring base" means the commit that defines the lower end of the candidate diff subject for a revision. For explicit seeding it is `seed_commit_oid`. For implicit seeding it is the first authoring workspace's `base_commit_oid`, captured from the current `target_ref` head at first mutating authoring dispatch.

## Plan of Work

Patch the revision semantics so `seed_commit_oid` becomes a nullable explicit override rather than an always-eager default. Patch the authoring workspace lifecycle so first provisioning for an implicit revision binds `base_commit_oid` from the current `target_ref` head and reuses that base thereafter. Then update all review subject, convergence replay, and revision-creation rules that currently assume `seed_commit_oid` is always the candidate base. Finish by updating the conformance matrix and reference algorithms so the summary sections match the normative text.

## Concrete Steps

From `/Users/aa/Documents/ingot`, edit `SPEC.md` and then inspect the result with:

    rg -n 'seed_commit_oid|base_commit_oid|bound authoring base|authoring workspace' SPEC.md
    git diff -- SPEC.md .agent/late-bound-authoring-base-spec.md

The diff should show the late-binding rule in the revision and workspace sections, and no remaining normative text should require implicit `seed_commit_oid` capture at revision creation.

## Validation and Acceptance

Acceptance is met when `SPEC.md` says all of the following:

1. `seed_commit_oid` may be null to represent implicit authoring-base binding.
2. implicit revisions bind their authoring base at the first mutating authoring dispatch, which creates the initial authoring workspace using the then-current `target_ref` head.
3. once bound, that authoring base stays fixed for later review and convergence on that revision.
4. initial item creation no longer requires defaulting `seed_commit_oid` from `target_ref` at revision creation.
5. superseding revisions still carry forward prior current authoring work by default when such work exists.

## Idempotence and Recovery

This is a documentation-only change. Reapplying the patch is safe. If wording becomes inconsistent, rerun the search commands above and reconcile all remaining references to eager `seed_commit_oid` defaults before stopping.

## Artifacts and Notes

Key intended rule after the patch:

    explicit seed_commit_oid -> authoring base is known at revision creation
    omitted seed_commit_oid -> authoring base binds at first mutating authoring dispatch
    once authoring base is bound -> review and convergence use that same base until superseded

## Interfaces and Dependencies

No code interfaces are introduced in this patch. The spec change is expected to affect the semantics of revision creation, workspace provisioning, job dispatch inputs, convergence replay, and report-only investigation anchor handling in later implementation work.

The focused TLA+ model in `formal/AuthoringBaseBinding.tla` now covers the pre-authoring `investigate_item` path, including creation of the durable anchor, triage of its findings, cleanup after triage, and coexistence with later authoring-base binding.

Revision note: created during the `SPEC.md` patch because the change alters multiple normative sections and needed an explicit design record tied to `.agent/PLANS.md`.
