-- Same advisory coordinate historically named buzz_channel_membership. Policy
-- generation subsumes that serialization boundary so old and new writers
-- contend on one key during rolling migration.
CREATE FUNCTION channel_policy_lock_key(target_community UUID, target_channel UUID)
RETURNS BIGINT
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT hashtextextended(
        'buzz_channel_membership:' || target_community::text || ':' || target_channel::text,
        0
    )
$$;
