ALTER TABLE convergences
ADD COLUMN checkout_adoption_state TEXT
    CHECK (
        checkout_adoption_state IS NULL
        OR checkout_adoption_state IN ('pending', 'blocked', 'synced')
    )
    CHECK (
        status != 'finalized'
        OR checkout_adoption_state IS NOT NULL
    );

ALTER TABLE convergences
ADD COLUMN checkout_adoption_message TEXT
    CHECK (
        checkout_adoption_state IS NULL
        OR checkout_adoption_state = 'blocked'
        OR checkout_adoption_message IS NULL
    )
    CHECK (
        checkout_adoption_state IS NULL
        OR checkout_adoption_state != 'blocked'
        OR checkout_adoption_message IS NOT NULL
    );

ALTER TABLE convergences
ADD COLUMN checkout_adoption_updated_at TEXT
    CHECK (
        status != 'finalized'
        OR checkout_adoption_updated_at IS NOT NULL
    );

ALTER TABLE convergences
ADD COLUMN checkout_adoption_synced_at TEXT
    CHECK (
        checkout_adoption_state IS NULL
        OR checkout_adoption_state = 'synced'
        OR checkout_adoption_synced_at IS NULL
    )
    CHECK (
        checkout_adoption_state IS NULL
        OR checkout_adoption_state != 'synced'
        OR checkout_adoption_synced_at IS NOT NULL
    );

UPDATE convergences
SET
    checkout_adoption_state = 'synced',
    checkout_adoption_updated_at = COALESCE(completed_at, created_at),
    checkout_adoption_synced_at = COALESCE(completed_at, created_at)
WHERE status = 'finalized';
