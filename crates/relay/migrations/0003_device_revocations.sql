-- Desired-state revocations are committed before registry-file reconciliation.
-- IDs are metadata; no keys, APNs tokens or content are copied here.
CREATE TABLE management_device_revocations (
    account_id UUID NOT NULL REFERENCES management_accounts(id),
    kind TEXT NOT NULL CHECK(kind IN ('mac','client')),
    device_id TEXT NOT NULL CHECK(octet_length(device_id) BETWEEN 1 AND 128),
    mac_id TEXT NOT NULL CHECK(octet_length(mac_id) BETWEEN 1 AND 128),
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','completed')),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY(kind,device_id)
);
ALTER TABLE management_audit DROP CONSTRAINT management_audit_action_check;
ALTER TABLE management_audit ADD CHECK(action IN ('bootstrap','invite_create','invite_cancel','invite_accept','account_disable','account_delete','device_revoke'));
