-- Monotone IFC policy coordinate retained after channel deletion. No foreign
-- key points at channels: retaining this row prevents a deleted UUID from ever
-- acquiring a fresh generation sequence inside the same community.
CREATE TABLE channel_policy_generations (
    community_id UUID NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    channel_id UUID NOT NULL,
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation > 0),
    deleted BOOLEAN NOT NULL DEFAULT FALSE,
    created_xid xid8 NOT NULL DEFAULT pg_current_xact_id(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, channel_id)
);
