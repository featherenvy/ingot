---- MODULE IngotControl ----
EXTENDS Naturals

\* A deliberately small control model for the safety-critical state coupling in
\* SPEC.md: approval, convergence, target-ref drift, and terminal closure.

CONSTANT MaxTargetVersion

ASSUME MaxTargetVersion \in Nat /\ MaxTargetVersion >= 1

ItemStates == {"open", "done"}
ApprovalPolicies == {"required", "not_required"}
ApprovalStates == {"not_requested", "not_required", "pending", "approved"}
ConvergenceStates == {"none", "running", "prepared", "finalized", "failed", "conflicted"}
ResolutionSources == {"none", "system_command", "approval_command"}
NoTarget == MaxTargetVersion + 1

VARIABLES
  itemState,
  approvalPolicy,
  approvalState,
  activeJob,
  convergenceState,
  targetRef,
  convInputTarget,
  resolutionSource

vars ==
  << itemState,
     approvalPolicy,
     approvalState,
     activeJob,
     convergenceState,
     targetRef,
     convInputTarget,
     resolutionSource >>

ApprovalReset(policy) ==
  IF policy = "required" THEN "not_requested" ELSE "not_required"

PreparedStillValid ==
  /\ convergenceState = "prepared"
  /\ targetRef = convInputTarget

Init ==
  /\ itemState = "open"
  /\ approvalPolicy \in ApprovalPolicies
  /\ approvalState = ApprovalReset(approvalPolicy)
  /\ activeJob = FALSE
  /\ convergenceState = "none"
  /\ targetRef = 0
  /\ convInputTarget = NoTarget
  /\ resolutionSource = "none"

DispatchJob ==
  /\ itemState = "open"
  /\ ~activeJob
  /\ convergenceState \in {"none", "failed", "conflicted"}
  /\ approvalState # "pending"
  /\ activeJob' = TRUE
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          approvalState,
          convergenceState,
          targetRef,
          convInputTarget,
          resolutionSource >>

CompleteJob ==
  /\ activeJob
  /\ activeJob' = FALSE
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          approvalState,
          convergenceState,
          targetRef,
          convInputTarget,
          resolutionSource >>

StartPrepare ==
  /\ itemState = "open"
  /\ ~activeJob
  /\ convergenceState \in {"none", "failed", "conflicted"}
  /\ approvalState # "pending"
  /\ convergenceState' = "running"
  /\ convInputTarget' = targetRef
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          approvalState,
          activeJob,
          targetRef,
          resolutionSource >>

PrepareSuccess ==
  /\ convergenceState = "running"
  /\ convergenceState' = "prepared"
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          approvalState,
          activeJob,
          targetRef,
          convInputTarget,
          resolutionSource >>

PrepareConflict ==
  /\ convergenceState = "running"
  /\ convergenceState' = "conflicted"
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          approvalState,
          activeJob,
          targetRef,
          convInputTarget,
          resolutionSource >>

IntegratedFindings ==
  /\ convergenceState = "prepared"
  /\ convergenceState' = "failed"
  /\ approvalState' = ApprovalReset(approvalPolicy)
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          activeJob,
          targetRef,
          convInputTarget,
          resolutionSource >>

RequestApproval ==
  /\ itemState = "open"
  /\ ~activeJob
  /\ approvalPolicy = "required"
  /\ approvalState = "not_requested"
  /\ PreparedStillValid
  /\ approvalState' = "pending"
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          activeJob,
          convergenceState,
          targetRef,
          convInputTarget,
          resolutionSource >>

Approve ==
  /\ itemState = "open"
  /\ ~activeJob
  /\ approvalPolicy = "required"
  /\ approvalState = "pending"
  /\ PreparedStillValid
  /\ itemState' = "done"
  /\ approvalState' = "approved"
  /\ convergenceState' = "finalized"
  /\ resolutionSource' = "approval_command"
  /\ UNCHANGED
       << approvalPolicy,
          activeJob,
          targetRef,
          convInputTarget >>

FinalizeWithoutApproval ==
  /\ itemState = "open"
  /\ ~activeJob
  /\ approvalPolicy = "not_required"
  /\ approvalState = "not_required"
  /\ PreparedStillValid
  /\ itemState' = "done"
  /\ convergenceState' = "finalized"
  /\ resolutionSource' = "system_command"
  /\ UNCHANGED
       << approvalPolicy,
          approvalState,
          activeJob,
          targetRef,
          convInputTarget >>

RejectApproval ==
  /\ itemState = "open"
  /\ approvalPolicy = "required"
  /\ approvalState = "pending"
  /\ convergenceState = "prepared"
  /\ approvalState' = "not_requested"
  /\ convergenceState' = "failed"
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          activeJob,
          targetRef,
          convInputTarget,
          resolutionSource >>

InvalidatePrepared ==
  /\ itemState = "open"
  /\ convergenceState = "prepared"
  /\ targetRef # convInputTarget
  /\ convergenceState' = "failed"
  /\ approvalState' = ApprovalReset(approvalPolicy)
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          activeJob,
          targetRef,
          convInputTarget,
          resolutionSource >>

TargetRefMoves ==
  /\ itemState = "open"
  /\ targetRef < MaxTargetVersion
  /\ targetRef' = targetRef + 1
  /\ IF convergenceState = "prepared"
        THEN /\ convergenceState' = "failed"
             /\ approvalState' = ApprovalReset(approvalPolicy)
        ELSE /\ convergenceState' = convergenceState
             /\ approvalState' = approvalState
  /\ UNCHANGED
       << itemState,
          approvalPolicy,
          activeJob,
          convInputTarget,
          resolutionSource >>

LateCallbackNoOp ==
  /\ itemState = "done"
  /\ UNCHANGED vars

Next ==
  \/ DispatchJob
  \/ CompleteJob
  \/ StartPrepare
  \/ PrepareSuccess
  \/ PrepareConflict
  \/ IntegratedFindings
  \/ RequestApproval
  \/ Approve
  \/ FinalizeWithoutApproval
  \/ RejectApproval
  \/ InvalidatePrepared
  \/ TargetRefMoves
  \/ LateCallbackNoOp

Spec ==
  Init /\ [][Next]_vars

TypeOK ==
  /\ itemState \in ItemStates
  /\ approvalPolicy \in ApprovalPolicies
  /\ approvalState \in ApprovalStates
  /\ activeJob \in BOOLEAN
  /\ convergenceState \in ConvergenceStates
  /\ targetRef \in 0..MaxTargetVersion
  /\ convInputTarget \in 0..NoTarget
  /\ resolutionSource \in ResolutionSources

PendingRequiresValidPrepared ==
  approvalState = "pending" =>
    /\ itemState = "open"
    /\ ~activeJob
    /\ convergenceState = "prepared"
    /\ targetRef = convInputTarget

ApprovedImpliesDone ==
  approvalState = "approved" =>
    /\ itemState = "done"
    /\ convergenceState = "finalized"
    /\ resolutionSource = "approval_command"

NotRequiredNeverPending ==
  approvalPolicy = "not_required" => approvalState # "pending"

DoneImpliesQuiescent ==
  itemState = "done" =>
    /\ ~activeJob
    /\ convergenceState = "finalized"
    /\ resolutionSource \in {"system_command", "approval_command"}

FinalizedImpliesDone ==
  convergenceState = "finalized" =>
    /\ itemState = "done"
    /\ resolutionSource \in {"system_command", "approval_command"}

====
