-- Private-alpha waitlist. A request grants no account, identity, invitation or access.
CREATE TABLE management_waitlist (
    id UUID PRIMARY KEY,
    seq BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    email TEXT NOT NULL CHECK(octet_length(email) BETWEEN 3 AND 254 AND email = lower(email) AND email !~ '[[:space:][:cntrl:]]'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT management_waitlist_email_key UNIQUE(email)
);
ALTER TABLE management_audit DROP CONSTRAINT management_audit_action_check;
ALTER TABLE management_audit ADD CHECK(action IN ('bootstrap','invite_create','invite_cancel','invite_accept','account_disable','account_delete','device_revoke','device_label','waitlist_remove'));
