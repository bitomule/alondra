-- Product metadata only. No device private keys or task content belong here.
CREATE TABLE management_accounts (
    id UUID PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('provisioning','active','disabled','deleting','deleted')),
    is_admin BOOLEAN NOT NULL DEFAULT FALSE,
    display_name TEXT NOT NULL CHECK (octet_length(display_name) <= 256),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE management_identities (
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    account_id UUID NOT NULL REFERENCES management_accounts(id),
    PRIMARY KEY (issuer,subject),
    UNIQUE (account_id,issuer)
);
CREATE TABLE management_invitations (
    id UUID PRIMARY KEY,
    email TEXT NOT NULL CHECK (email = lower(email) AND octet_length(email) <= 254),
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    status TEXT NOT NULL CHECK (status IN ('pending','provisioning','accepted','canceled')),
    account_id UUID UNIQUE REFERENCES management_accounts(id),
    expires_at TIMESTAMPTZ NOT NULL,
    created_by UUID NOT NULL REFERENCES management_accounts(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    accepted_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX management_invitation_live_email ON management_invitations(email)
    WHERE status IN ('pending','provisioning');
CREATE TABLE management_audit (
    id BIGSERIAL PRIMARY KEY,
    actor_id UUID REFERENCES management_accounts(id),
    action TEXT NOT NULL CHECK (action IN ('bootstrap','invite_create','invite_cancel','invite_accept','account_disable','account_delete')),
    target_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE management_outbox (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES management_accounts(id),
    action TEXT NOT NULL CHECK (action IN ('keycloak_disable','keycloak_delete','relay_revoke')),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','completed')),
    attempts BIGINT NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    UNIQUE (account_id,action)
);

CREATE TABLE management_admin_requests (
    actor_id UUID NOT NULL REFERENCES management_accounts(id),
    key_hash BYTEA NOT NULL CHECK (octet_length(key_hash)=32),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash)=32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(actor_id,key_hash)
);
