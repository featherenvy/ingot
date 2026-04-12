UPDATE convergences
SET
    checkout_adoption_state = 'pending',
    checkout_adoption_message = NULL,
    checkout_adoption_updated_at = COALESCE(completed_at, created_at),
    checkout_adoption_synced_at = NULL
WHERE status = 'finalized'
  AND id IN (
      SELECT entity_id
      FROM git_operations
      WHERE operation_kind = 'finalize_target_ref'
        AND entity_type = 'convergence'
        AND status IN ('planned', 'applied')
  );
