-- Canonical monotone channel policy generations and durable fenced-event
-- receipts for remote-agent IFC version 1.

ALTER TABLE channels
    ADD COLUMN history_access TEXT NOT NULL DEFAULT 'all_current_members',
    ADD CONSTRAINT chk_channels_history_access
        CHECK (history_access = 'all_current_members');

CREATE TABLE channel_policy_generations (
    community_id UUID NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    channel_id UUID NOT NULL,
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation > 0),
    deleted BOOLEAN NOT NULL DEFAULT FALSE,
    created_xid xid8 NOT NULL DEFAULT pg_current_xact_id(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, channel_id)
);

INSERT INTO channel_policy_generations
    (community_id, channel_id, generation, deleted, created_xid)
SELECT community_id, id, 1, deleted_at IS NOT NULL, pg_current_xact_id()
FROM channels;

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

CREATE FUNCTION channel_policy_lock_key(target_community UUID, target_channel UUID)
RETURNS BIGINT
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT hashtextextended(
        'buzz_channel_membership:' || target_community::text || ':' || target_channel::text,
        0
    )
$$;

CREATE FUNCTION initialize_channel_policy_generation() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target RECORD;
BEGIN
    FOR target IN
        SELECT community_id, id AS channel_id
        FROM new_channels
        ORDER BY community_id, id
    LOOP
        INSERT INTO channel_policy_generations
            (community_id, channel_id, generation, deleted, created_xid)
        VALUES
            (target.community_id, target.channel_id, 1, FALSE, pg_current_xact_id())
        ON CONFLICT (community_id, channel_id) DO NOTHING;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'deleted channel identity cannot be reused: %/%',
                target.community_id, target.channel_id
                USING ERRCODE = '23514';
        END IF;
    END LOOP;
    RETURN NULL;
END
$$;

CREATE FUNCTION channel_member_policy_identity_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.community_id, NEW.channel_id, NEW.pubkey)
       IS DISTINCT FROM ROW(OLD.community_id, OLD.channel_id, OLD.pubkey) THEN
        RAISE EXCEPTION 'channel membership identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;

CREATE FUNCTION channel_policy_identity_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.community_id, NEW.id)
       IS DISTINCT FROM ROW(OLD.community_id, OLD.id) THEN
        RAISE EXCEPTION 'channel policy identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;

CREATE FUNCTION guard_channel_policy_generation_row() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF pg_trigger_depth() <= 1 THEN
            RAISE EXCEPTION 'channel policy generations cannot be deleted directly'
                USING ERRCODE = '23514';
        END IF;
        RETURN OLD;
    END IF;
    IF ROW(NEW.community_id, NEW.channel_id, NEW.created_xid)
       IS DISTINCT FROM ROW(OLD.community_id, OLD.channel_id, OLD.created_xid)
       OR NEW.generation < OLD.generation
       OR (OLD.deleted AND NOT NEW.deleted) THEN
        RAISE EXCEPTION 'channel policy generation is monotone and identity-bound'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;

CREATE FUNCTION guard_ifc_acceptance_row() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' OR pg_trigger_depth() <= 1 THEN
        RAISE EXCEPTION 'IFC acceptance receipts are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;

CREATE FUNCTION advance_channel_policy_for_channel_update() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target RECORD;
BEGIN
    FOR target IN
        SELECT n.community_id, n.id AS channel_id, n.deleted_at IS NOT NULL AS deleted
        FROM new_channels n
        JOIN old_channels o USING (community_id, id)
        WHERE ROW(n.channel_type, n.visibility, n.history_access, n.created_by,
                  n.archived_at, n.deleted_at)
              IS DISTINCT FROM
              ROW(o.channel_type, o.visibility, o.history_access, o.created_by,
                  o.archived_at, o.deleted_at)
        ORDER BY n.community_id, n.id
    LOOP
        PERFORM pg_advisory_xact_lock(
            channel_policy_lock_key(target.community_id, target.channel_id));
        UPDATE channel_policy_generations
        SET generation = generation + 1,
            deleted = target.deleted,
            updated_at = NOW()
        WHERE community_id = target.community_id
          AND channel_id = target.channel_id;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'channel policy generation missing: %/%',
                target.community_id, target.channel_id
                USING ERRCODE = '23514';
        END IF;
    END LOOP;
    RETURN NULL;
END
$$;

CREATE FUNCTION advance_channel_policy_for_channel_delete() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target RECORD;
BEGIN
    FOR target IN
        SELECT community_id, id AS channel_id
        FROM old_channels
        ORDER BY community_id, id
    LOOP
        PERFORM pg_advisory_xact_lock(
            channel_policy_lock_key(target.community_id, target.channel_id));
        UPDATE channel_policy_generations
        SET generation = generation + 1,
            deleted = TRUE,
            updated_at = NOW()
        WHERE community_id = target.community_id
          AND channel_id = target.channel_id;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'channel policy generation missing: %/%',
                target.community_id, target.channel_id
                USING ERRCODE = '23514';
        END IF;
    END LOOP;
    RETURN NULL;
END
$$;

CREATE FUNCTION advance_channel_policy_for_member_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target RECORD;
BEGIN
    FOR target IN
        SELECT DISTINCT n.community_id, n.channel_id
        FROM new_members n
        JOIN channel_policy_generations p
          ON p.community_id = n.community_id AND p.channel_id = n.channel_id
        WHERE n.removed_at IS NULL
          AND p.deleted = FALSE
          AND p.created_xid <> pg_current_xact_id()
        ORDER BY n.community_id, n.channel_id
    LOOP
        PERFORM pg_advisory_xact_lock(
            channel_policy_lock_key(target.community_id, target.channel_id));
        UPDATE channel_policy_generations
        SET generation = generation + 1, updated_at = NOW()
        WHERE community_id = target.community_id
          AND channel_id = target.channel_id
          AND deleted = FALSE;
    END LOOP;
    RETURN NULL;
END
$$;

CREATE FUNCTION advance_channel_policy_for_member_update() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target RECORD;
BEGIN
    FOR target IN
        WITH changed AS (
            SELECT o.community_id AS old_community, o.channel_id AS old_channel,
                   o.role AS old_role, o.removed_at IS NULL AS old_active,
                   n.community_id AS new_community, n.channel_id AS new_channel,
                   n.role AS new_role, n.removed_at IS NULL AS new_active
            FROM old_members o
            JOIN new_members n
              ON o.community_id = n.community_id
             AND o.channel_id = n.channel_id
             AND o.pubkey = n.pubkey
            WHERE (o.removed_at IS NULL) IS DISTINCT FROM (n.removed_at IS NULL)
               OR ((o.removed_at IS NULL) AND (n.removed_at IS NULL)
                   AND o.role IS DISTINCT FROM n.role)
        ), affected AS (
            SELECT old_community AS community_id, old_channel AS channel_id
            FROM changed WHERE old_active
            UNION
            SELECT new_community, new_channel FROM changed WHERE new_active
        )
        SELECT community_id, channel_id FROM affected
        ORDER BY community_id, channel_id
    LOOP
        PERFORM pg_advisory_xact_lock(
            channel_policy_lock_key(target.community_id, target.channel_id));
        UPDATE channel_policy_generations
        SET generation = generation + 1, updated_at = NOW()
        WHERE community_id = target.community_id
          AND channel_id = target.channel_id
          AND deleted = FALSE;
    END LOOP;
    RETURN NULL;
END
$$;

CREATE FUNCTION advance_channel_policy_for_member_delete() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    target RECORD;
BEGIN
    FOR target IN
        SELECT DISTINCT o.community_id, o.channel_id
        FROM old_members o
        JOIN channel_policy_generations p
          ON p.community_id = o.community_id AND p.channel_id = o.channel_id
        WHERE o.removed_at IS NULL AND p.deleted = FALSE
        ORDER BY o.community_id, o.channel_id
    LOOP
        PERFORM pg_advisory_xact_lock(
            channel_policy_lock_key(target.community_id, target.channel_id));
        UPDATE channel_policy_generations
        SET generation = generation + 1, updated_at = NOW()
        WHERE community_id = target.community_id
          AND channel_id = target.channel_id
          AND deleted = FALSE;
    END LOOP;
    RETURN NULL;
END
$$;

CREATE FUNCTION enforce_ifc_acceptance_expiry() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.fence_expires_at <= clock_timestamp() THEN
        RAISE EXCEPTION 'ifc-expired'
            USING ERRCODE = '23514', CONSTRAINT = 'ifc_acceptance_unexpired';
    END IF;
    RETURN NULL;
END
$$;

CREATE FUNCTION prune_ifc_acceptances_after_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM ifc_event_acceptances
    WHERE ctid IN (
        SELECT a.ctid
        FROM ifc_event_acceptances a
        JOIN (SELECT DISTINCT community_id FROM new_acceptances) n
          ON n.community_id = a.community_id
        WHERE a.accepted_at < clock_timestamp() - INTERVAL '24 hours'
        ORDER BY a.accepted_at
        LIMIT 1000
    );
    RETURN NULL;
END
$$;

CREATE TRIGGER trg_channels_ifc_policy_insert
    AFTER INSERT ON channels
    REFERENCING NEW TABLE AS new_channels
    FOR EACH STATEMENT EXECUTE FUNCTION initialize_channel_policy_generation();
CREATE TRIGGER trg_channels_ifc_policy_identity
    BEFORE UPDATE ON channels
    FOR EACH ROW EXECUTE FUNCTION channel_policy_identity_immutable();
CREATE TRIGGER trg_channels_ifc_policy_update
    AFTER UPDATE ON channels
    REFERENCING OLD TABLE AS old_channels NEW TABLE AS new_channels
    FOR EACH STATEMENT EXECUTE FUNCTION advance_channel_policy_for_channel_update();
CREATE TRIGGER trg_channels_ifc_policy_delete
    AFTER DELETE ON channels
    REFERENCING OLD TABLE AS old_channels
    FOR EACH STATEMENT EXECUTE FUNCTION advance_channel_policy_for_channel_delete();
CREATE TRIGGER trg_channel_members_ifc_policy_insert
    AFTER INSERT ON channel_members
    REFERENCING NEW TABLE AS new_members
    FOR EACH STATEMENT EXECUTE FUNCTION advance_channel_policy_for_member_insert();
CREATE TRIGGER trg_channel_members_ifc_policy_identity
    BEFORE UPDATE ON channel_members
    FOR EACH ROW EXECUTE FUNCTION channel_member_policy_identity_immutable();
CREATE TRIGGER trg_channel_members_ifc_policy_update
    AFTER UPDATE ON channel_members
    REFERENCING OLD TABLE AS old_members NEW TABLE AS new_members
    FOR EACH STATEMENT EXECUTE FUNCTION advance_channel_policy_for_member_update();
CREATE TRIGGER trg_channel_members_ifc_policy_delete
    AFTER DELETE ON channel_members
    REFERENCING OLD TABLE AS old_members
    FOR EACH STATEMENT EXECUTE FUNCTION advance_channel_policy_for_member_delete();

CREATE CONSTRAINT TRIGGER trg_ifc_acceptance_unexpired
    AFTER INSERT ON ifc_event_acceptances
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION enforce_ifc_acceptance_expiry();

CREATE TRIGGER trg_ifc_acceptance_prune
    AFTER INSERT ON ifc_event_acceptances
    REFERENCING NEW TABLE AS new_acceptances
    FOR EACH STATEMENT EXECUTE FUNCTION prune_ifc_acceptances_after_insert();

CREATE TRIGGER trg_channel_policy_generation_guard
    BEFORE UPDATE OR DELETE ON channel_policy_generations
    FOR EACH ROW EXECUTE FUNCTION guard_channel_policy_generation_row();
CREATE TRIGGER trg_ifc_acceptance_guard
    BEFORE UPDATE OR DELETE ON ifc_event_acceptances
    FOR EACH ROW EXECUTE FUNCTION guard_ifc_acceptance_row();

SELECT attach_community_write_fence('channel_policy_generations');
SELECT attach_community_write_fence('ifc_event_acceptances');
