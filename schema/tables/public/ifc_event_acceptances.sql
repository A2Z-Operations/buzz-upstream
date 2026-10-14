-- Durable accepted-event receipt. It is intentionally independent of events
-- retention so an exact signed retry cannot become a new write after expiry.
CREATE TABLE ifc_event_acceptances (
    community_id UUID NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    event_id BYTEA NOT NULL CHECK (octet_length(event_id) = 32),
    event_kind INT NOT NULL,
    fence_expires_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, event_id)
);

CREATE INDEX idx_ifc_event_acceptances_accepted_at
    ON ifc_event_acceptances (accepted_at);
