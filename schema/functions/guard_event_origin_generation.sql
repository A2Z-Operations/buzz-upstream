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
