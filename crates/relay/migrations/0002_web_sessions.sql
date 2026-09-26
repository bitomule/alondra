-- Browser capabilities are random opaque cookies. OIDC tokens and login-flow
-- secrets are AEAD-encrypted with a key held outside PostgreSQL and its backups.
CREATE TABLE web_login_flows (
    cookie_hash BYTEA PRIMARY KEY CHECK (octet_length(cookie_hash)=32),
    state_hash BYTEA NOT NULL CHECK (octet_length(state_hash)=32),
    sealed BYTEA NOT NULL,
    expires_ms BIGINT NOT NULL
);
CREATE INDEX web_login_flows_expiry ON web_login_flows(expires_ms);
CREATE TABLE web_sessions (
    cookie_hash BYTEA PRIMARY KEY CHECK (octet_length(cookie_hash)=32),
    account_id UUID NOT NULL REFERENCES management_accounts(id),
    subject TEXT NOT NULL,
    sealed BYTEA NOT NULL,
    created_ms BIGINT NOT NULL,
    used_ms BIGINT NOT NULL,
    expires_ms BIGINT NOT NULL,
    access_expires_ms BIGINT NOT NULL,
    refresh_state TEXT NOT NULL DEFAULT 'ready' CHECK (refresh_state IN ('ready','refreshing','revoked')),
    revision BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX web_sessions_account ON web_sessions(account_id);
CREATE INDEX web_sessions_expiry ON web_sessions(expires_ms);
