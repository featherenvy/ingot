ALTER TABLE projects ADD COLUMN execution_mode TEXT NOT NULL DEFAULT 'manual'
    CHECK (execution_mode IN ('manual', 'autopilot'));
