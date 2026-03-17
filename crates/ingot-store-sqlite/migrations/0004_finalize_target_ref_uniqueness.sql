CREATE UNIQUE INDEX idx_git_ops_active_finalize_per_convergence
    ON git_operations(project_id, entity_id)
    WHERE operation_kind = 'finalize_target_ref'
      AND entity_type = 'convergence'
      AND status IN ('planned', 'applied');
