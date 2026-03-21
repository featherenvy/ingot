ALTER TABLE items ADD COLUMN sort_key TEXT NOT NULL DEFAULT '';
UPDATE items SET sort_key = created_at;
