---- MODULE AuthoringBaseBinding ----
EXTENDS Naturals

\* A deliberately small control model for a revision whose authoring base may be
\* bound either explicitly at revision creation or implicitly when authoring
\* first starts. The intent is to model the SPEC.md change where implicit work
\* begins on the latest target head at first mutating authoring dispatch, but
\* the resulting authoring base is then frozen for all later review and
\* convergence work. The model also covers the pre-authoring investigate_item
\* path, including retention of the durable local commit reference used for an
\* untriaged pre-authoring investigation subject.

CONSTANT MaxTargetVersion, MaxAuthoringCommits

ASSUME /\ MaxTargetVersion \in Nat
       /\ MaxTargetVersion >= 1
       /\ MaxAuthoringCommits \in Nat
       /\ MaxAuthoringCommits >= 1

SeedModes == {"explicit", "implicit"}
ConvergenceStates == {"none", "running", "prepared", "conflicted", "failed", "finalized"}
NoCommit == MaxTargetVersion + 1

VARIABLES
  seedMode,
  targetRef,
  authoringBase,
  baseBindingCount,
  activeAuthoringJob,
  authoringCommitCount,
  preInvestigationAnchor,
  preInvestigationFindingsOpen,
  lastReviewBase,
  convergenceState,
  convSourceBase,
  convInputTarget

vars ==
  << seedMode,
     targetRef,
     authoringBase,
     baseBindingCount,
     activeAuthoringJob,
     authoringCommitCount,
     preInvestigationAnchor,
     preInvestigationFindingsOpen,
     lastReviewBase,
     convergenceState,
     convSourceBase,
     convInputTarget >>

BaseBound ==
  authoringBase # NoCommit

Init ==
  /\ seedMode \in SeedModes
  /\ targetRef = 0
  /\ IF seedMode = "explicit"
        THEN /\ authoringBase = 0
             /\ baseBindingCount = 1
        ELSE /\ authoringBase = NoCommit
             /\ baseBindingCount = 0
  /\ activeAuthoringJob = FALSE
  /\ authoringCommitCount = 0
  /\ preInvestigationAnchor = NoCommit
  /\ preInvestigationFindingsOpen = FALSE
  /\ lastReviewBase = NoCommit
  /\ convergenceState = "none"
  /\ convSourceBase = NoCommit
  /\ convInputTarget = NoCommit

TargetRefMoves ==
  /\ targetRef < MaxTargetVersion
  /\ targetRef' = targetRef + 1
  /\ IF convergenceState = "prepared"
        THEN convergenceState' = "failed"
        ELSE convergenceState' = convergenceState
  /\ UNCHANGED
       << seedMode,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convSourceBase,
          convInputTarget >>

StartAuthoring ==
  /\ ~activeAuthoringJob
  /\ authoringCommitCount < MaxAuthoringCommits
  /\ convergenceState \in {"none", "failed", "conflicted"}
  /\ IF seedMode = "implicit" /\ ~BaseBound
        THEN /\ authoringBase' = targetRef
             /\ baseBindingCount' = baseBindingCount + 1
        ELSE /\ authoringBase' = authoringBase
             /\ baseBindingCount' = baseBindingCount
  /\ activeAuthoringJob' = TRUE
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convergenceState,
          convSourceBase,
          convInputTarget >>

CompleteAuthoring ==
  /\ activeAuthoringJob
  /\ BaseBound
  /\ activeAuthoringJob' = FALSE
  /\ authoringCommitCount' = authoringCommitCount + 1
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convergenceState,
          convSourceBase,
          convInputTarget >>

InvestigatePreAuthoringFindings ==
  /\ ~activeAuthoringJob
  /\ seedMode = "implicit"
  /\ ~BaseBound
  /\ authoringCommitCount = 0
  /\ ~preInvestigationFindingsOpen
  /\ preInvestigationAnchor = NoCommit
  /\ convergenceState \in {"none", "failed", "conflicted"}
  /\ preInvestigationAnchor' = targetRef
  /\ preInvestigationFindingsOpen' = TRUE
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          lastReviewBase,
          convergenceState,
          convSourceBase,
          convInputTarget >>

TriagePreAuthoringFindings ==
  /\ preInvestigationFindingsOpen
  /\ preInvestigationFindingsOpen' = FALSE
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          lastReviewBase,
          convergenceState,
          convSourceBase,
          convInputTarget >>

CleanupPreAuthoringAnchor ==
  /\ ~preInvestigationFindingsOpen
  /\ preInvestigationAnchor # NoCommit
  /\ preInvestigationAnchor' = NoCommit
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convergenceState,
          convSourceBase,
          convInputTarget >>

ReviewCandidate ==
  /\ ~activeAuthoringJob
  /\ authoringCommitCount > 0
  /\ BaseBound
  /\ lastReviewBase' = authoringBase
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          convergenceState,
          convSourceBase,
          convInputTarget >>

StartPrepare ==
  /\ ~activeAuthoringJob
  /\ authoringCommitCount > 0
  /\ BaseBound
  /\ convergenceState \in {"none", "failed", "conflicted"}
  /\ convergenceState' = "running"
  /\ convSourceBase' = authoringBase
  /\ convInputTarget' = targetRef
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase >>

PrepareSuccess ==
  /\ convergenceState = "running"
  /\ targetRef = convInputTarget
  /\ convergenceState' = "prepared"
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convSourceBase,
          convInputTarget >>

PrepareStale ==
  /\ convergenceState = "running"
  /\ targetRef # convInputTarget
  /\ convergenceState' = "failed"
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convSourceBase,
          convInputTarget >>

PrepareConflict ==
  /\ convergenceState = "running"
  /\ convergenceState' = "conflicted"
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convSourceBase,
          convInputTarget >>

IntegratedFindings ==
  /\ convergenceState = "prepared"
  /\ convergenceState' = "failed"
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convSourceBase,
          convInputTarget >>

FinalizePrepared ==
  /\ convergenceState = "prepared"
  /\ targetRef = convInputTarget
  /\ convergenceState' = "finalized"
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convSourceBase,
          convInputTarget >>

InvalidatePrepared ==
  /\ convergenceState = "prepared"
  /\ targetRef # convInputTarget
  /\ convergenceState' = "failed"
  /\ UNCHANGED
       << seedMode,
          targetRef,
          authoringBase,
          baseBindingCount,
          activeAuthoringJob,
          authoringCommitCount,
          preInvestigationAnchor,
          preInvestigationFindingsOpen,
          lastReviewBase,
          convSourceBase,
          convInputTarget >>

Next ==
  \/ TargetRefMoves
  \/ StartAuthoring
  \/ CompleteAuthoring
  \/ InvestigatePreAuthoringFindings
  \/ TriagePreAuthoringFindings
  \/ CleanupPreAuthoringAnchor
  \/ ReviewCandidate
  \/ StartPrepare
  \/ PrepareSuccess
  \/ PrepareStale
  \/ PrepareConflict
  \/ IntegratedFindings
  \/ FinalizePrepared
  \/ InvalidatePrepared

Spec ==
  Init /\ [][Next]_vars

TypeOK ==
  /\ seedMode \in SeedModes
  /\ targetRef \in 0..MaxTargetVersion
  /\ authoringBase \in 0..NoCommit
  /\ baseBindingCount \in 0..1
  /\ activeAuthoringJob \in BOOLEAN
  /\ authoringCommitCount \in 0..MaxAuthoringCommits
  /\ preInvestigationAnchor \in 0..NoCommit
  /\ preInvestigationFindingsOpen \in BOOLEAN
  /\ lastReviewBase \in 0..NoCommit
  /\ convergenceState \in ConvergenceStates
  /\ convSourceBase \in 0..NoCommit
  /\ convInputTarget \in 0..NoCommit

BindingCountMatchesBase ==
  /\ (baseBindingCount = 0) => authoringBase = NoCommit
  /\ (authoringBase = NoCommit) => baseBindingCount = 0
  /\ BaseBound => baseBindingCount = 1

UnboundOnlyForImplicitBeforeWork ==
  authoringBase = NoCommit =>
    /\ seedMode = "implicit"
    /\ authoringCommitCount = 0
    /\ ~activeAuthoringJob
    /\ convergenceState = "none"

PreInvestigationFindingsRequireAnchor ==
  preInvestigationFindingsOpen =>
    /\ preInvestigationAnchor # NoCommit
    /\ seedMode = "implicit"

PreInvestigationAnchorClearsOnlyAfterTriage ==
  preInvestigationAnchor = NoCommit => ~preInvestigationFindingsOpen

AuthoringExecutionRequiresBoundBase ==
  activeAuthoringJob => BaseBound

AuthoringWorkRequiresBoundBase ==
  authoringCommitCount > 0 => BaseBound

ReviewUsesFrozenBase ==
  lastReviewBase # NoCommit => lastReviewBase = authoringBase

PrepareUsesFrozenBase ==
  convergenceState # "none" =>
    /\ convSourceBase = authoringBase
    /\ convSourceBase # NoCommit
    /\ convInputTarget # NoCommit

PreparedRequiresStableTarget ==
  convergenceState = "prepared" => targetRef = convInputTarget

====
