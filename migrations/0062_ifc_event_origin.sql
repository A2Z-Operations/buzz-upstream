-- Durable relay-authenticated destination generation for newly fenced channel
-- and canvas publications. Legacy and ordinary publications remain unknown.

ALTER TABLE events
    ADD COLUMN origin_generation BIGINT,
    ADD CONSTRAINT chk_events_origin_generation
        CHECK (origin_generation IS NULL OR origin_generation > 0);

CREATE FUNCTION guard_event_origin_generation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.origin_generation IS NOT NULL THEN
        RAISE EXCEPTION 'event origin generation must be assigned after fenced validation'
            USING ERRCODE = '23514';
    END IF;
    IF TG_OP = 'UPDATE'
       AND OLD.origin_generation IS NOT NULL
       AND NEW.origin_generation IS DISTINCT FROM OLD.origin_generation THEN
        RAISE EXCEPTION 'event origin generation is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER trg_events_guard_origin_generation
    BEFORE INSERT OR UPDATE OF origin_generation ON events
    FOR EACH ROW EXECUTE FUNCTION guard_event_origin_generation();
