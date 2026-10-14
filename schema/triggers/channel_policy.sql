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
