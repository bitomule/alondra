-- Account-scoped display aliases only; never alter registry IDs, keys or trust.
CREATE TABLE management_device_labels (
    account_id UUID NOT NULL REFERENCES management_accounts(id),
    kind TEXT NOT NULL CHECK(kind IN ('mac','client')),
    device_id TEXT NOT NULL CHECK(octet_length(device_id) BETWEEN 1 AND 128),
    mac_id TEXT NOT NULL CHECK(octet_length(mac_id) BETWEEN 1 AND 128),
    label TEXT CHECK(label IS NULL OR (char_length(label) BETWEEN 1 AND 128 AND label !~ '[[:cntrl:]]')),
    revision BIGINT NOT NULL CHECK(revision > 0),
    CHECK(kind != 'mac' OR device_id=mac_id),
    PRIMARY KEY(account_id,kind,device_id)
);
ALTER TABLE management_audit DROP CONSTRAINT management_audit_action_check;
ALTER TABLE management_audit ADD CHECK(action IN ('bootstrap','invite_create','invite_cancel','invite_accept','account_disable','account_delete','device_revoke','device_label'));
