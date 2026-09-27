use super::*;

// Run only against a disposable local PostgreSQL database. Each test gets its own
// schema; concurrency assertions exercise PostgreSQL locks, not SQLite substitutes.
async fn fixture() -> Result<(Management, Account, String)> {
    fixture_for_issuer("https://identity.example/realms/test").await
}
pub(crate) async fn fixture_for_issuer(issuer: &str) -> Result<(Management, Account, String)> {
    fixture_for_identity(issuer, "stable-admin-sub").await
}
pub(crate) async fn fixture_for_identity(
    issuer: &str,
    subject: &str,
) -> Result<(Management, Account, String)> {
    let (management, schema) = empty_fixture(issuer).await?;
    let pool = &management.pool;
    sqlx::raw_sql(include_str!("../../migrations/0001_management.sql"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!("../../migrations/0003_device_revocations.sql"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!("../../migrations/0004_device_labels.sql"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!("../../migrations/0005_waitlist.sql"))
        .execute(pool)
        .await?;
    let actor = management.bootstrap(subject, "Admin").await?;
    Ok((management, actor, schema))
}
async fn empty_fixture(issuer: &str) -> Result<(Management, String)> {
    let url = std::env::var("ALONDRA_MANAGEMENT_TEST_DATABASE_URL")
        .context("set disposable local PostgreSQL URL")?;
    let parsed = reqwest::Url::parse(&url)?;
    ensure!(
        matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1")),
        "test DB must be loopback"
    );
    let schema = format!("management_test_{}", Uuid::new_v4().simple());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    sqlx::raw_sql(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await?;
    let schema_on_connect = schema.clone();
    let pool=PgPoolOptions::new().max_connections(6).after_connect(move|conn,_| {
        let schema=schema_on_connect.clone();Box::pin(async move {
            sqlx::query("SELECT set_config('search_path',$1,false), set_config('statement_timeout','3s',false)")
                .bind(schema).execute(conn).await?;Ok(())
        })
    }).connect(&url).await?;
    admin.close().await;
    let management = Management {
        pool,
        issuer: issuer.into(),
    };
    Ok((management, schema))
}
pub(crate) async fn cleanup(db: Management, schema: String) -> Result<()> {
    db.pool.close().await;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("ALONDRA_MANAGEMENT_TEST_DATABASE_URL")?)
        .await?;
    sqlx::raw_sql(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}
#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_invitation_secrets_reissue_and_single_use() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let invite = db
        .create_invitation(admin.id, "Friend@Example.com", 3600)
        .await?;
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM management_invitations WHERE id=$1")
            .bind(invite.invitation.id)
            .fetch_one(&db.pool)
            .await?;
    assert_eq!(stored, hash_token(&invite.token)?);
    assert_ne!(stored, invite.token.as_bytes());
    let next = db
        .reissue_invitation(admin.id, invite.invitation.id, 3600)
        .await?;
    assert!(
        db.reserve_acceptance(&invite.token, "Friend")
            .await
            .is_err()
    );
    let reserved = db.reserve_acceptance(&next.token, "Friend").await?;
    let account_id = reserved.account_id;
    assert!(
        db.reserve_acceptance(&next.token, "Other name")
            .await
            .is_err(),
        "parallel claimant cannot provision"
    );
    let accepted = reserved.complete("friend-sub").await?;
    assert_eq!(accepted.id, account_id);
    assert!(db.reserve_acceptance(&next.token, "Repeat").await.is_err());
    assert_eq!(
        db.active_identity(&db.issuer, "friend-sub").await?.id,
        account_id
    );
    assert!(
        db.active_identity("https://other.example", "friend-sub")
            .await
            .is_err()
    );
    assert!(
        db.create_invitation(account_id, "bad@example.com", 3600)
            .await
            .is_err()
    );
    let audits: Vec<String> = sqlx::query_scalar("SELECT action FROM management_audit")
        .fetch_all(&db.pool)
        .await?;
    assert_eq!(
        audits
            .iter()
            .filter(|a| a.as_str() == "invite_accept")
            .count(),
        1
    );
    cleanup(db, schema).await
}
#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_expiration_during_provisioning_blocks_activation_and_queues_cleanup()
-> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let invite = db
        .create_invitation(admin.id, "expiry@example.com", 300)
        .await?;
    let reserved = db.reserve_acceptance(&invite.token, "Friend").await?;
    let id = reserved.account_id;
    sqlx::query(
        "UPDATE management_invitations SET expires_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(invite.invitation.id)
    .execute(&db.pool)
    .await?;
    assert!(reserved.complete("expired-sub").await.is_err());
    assert!(db.active_identity(&db.issuer, "expired-sub").await.is_err());
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM management_outbox WHERE account_id=$1 ORDER BY action",
    )
    .bind(id)
    .fetch_all(&db.pool)
    .await?;
    assert_eq!(
        actions,
        vec!["keycloak_delete", "keycloak_disable", "relay_revoke"]
    );
    cleanup(db, schema).await
}
#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_disable_delete_retries_preserve_identity_and_atomic_cleanup() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let invite = db
        .create_invitation(admin.id, "friend@example.com", 3600)
        .await?;
    let user = db
        .reserve_acceptance(&invite.token, "Friend")
        .await?
        .complete("friend-sub")
        .await?;
    assert!(db.disable_account(admin.id, admin.id, true).await.is_err());
    sqlx::raw_sql("CREATE FUNCTION reject_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test failure'; END $$; CREATE TRIGGER reject_outbox BEFORE INSERT ON management_outbox FOR EACH ROW EXECUTE FUNCTION reject_outbox();")
        .execute(&db.pool).await?;
    assert!(db.disable_account(admin.id, user.id, true).await.is_err());
    assert!(
        db.active_identity(&db.issuer, "friend-sub").await.is_ok(),
        "failed enqueue rolls back lifecycle change"
    );
    sqlx::raw_sql("DROP TRIGGER reject_outbox ON management_outbox")
        .execute(&db.pool)
        .await?;
    assert_eq!(
        db.disable_account(admin.id, user.id, false).await?.status,
        "disabled"
    );
    assert!(db.active_identity(&db.issuer, "friend-sub").await.is_err());
    assert_eq!(
        db.disable_account(admin.id, user.id, true).await?.status,
        "deleting"
    );
    assert_eq!(
        db.disable_account(admin.id, user.id, true).await?.id,
        user.id
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM management_outbox WHERE account_id=$1")
            .bind(user.id)
            .fetch_one(&db.pool)
            .await?;
    assert_eq!(count, 3);
    let same = Management {
        pool: db.pool.clone(),
        issuer: db.issuer.clone(),
    };
    assert_eq!(
        same.bootstrap("stable-admin-sub", "Ignored new name")
            .await?
            .id,
        admin.id
    );
    assert!(same.bootstrap("different-admin", "Other").await.is_err());
    cleanup(db, schema).await
}
#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_concurrent_invitations_and_cancel_after_reservation() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let (a, b) = tokio::join!(
        db.create_invitation(admin.id, "same@example.com", 3600),
        db.create_invitation(admin.id, "same@example.com", 3600)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let invite = a.or(b)?;
    let reservation = db.reserve_acceptance(&invite.token, "Friend").await?;
    let original_id = reservation.account_id;
    drop(reservation);
    // Cancellation after a lost network response is authoritative; no token reuse.
    db.cancel_invitation(admin.id, invite.invitation.id).await?;
    assert!(
        db.reserve_acceptance(&invite.token, "Friend")
            .await
            .is_err()
    );
    let state: String = sqlx::query_scalar("SELECT status FROM management_accounts WHERE id=$1")
        .bind(original_id)
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(state, "deleting");
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM management_outbox WHERE account_id=$1 AND state='pending'",
    )
    .bind(original_id)
    .fetch_one(&db.pool)
    .await?;
    assert_eq!(pending, 3);
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_idempotency_commits_with_effect_and_never_recovers_plaintext() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let key = "client-request-000001";
    let request = AdminRequest::new(
        key,
        "invite",
        &serde_json::json!({"email":"one@example.com"}),
        true,
    )?;
    let (a, b) = tokio::join!(
        db.create_invitation_request(admin.id, "one@example.com", 300, Some(&request)),
        db.create_invitation_request(admin.id, "one@example.com", 300, Some(&request))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let error = if let Err(e) = a { e } else { b.err().unwrap() };
    assert_eq!(
        error.downcast_ref::<DomainError>(),
        Some(&DomainError::TokenAlreadyIssued)
    );
    let changed = AdminRequest::new(
        key,
        "invite",
        &serde_json::json!({"email":"two@example.com"}),
        true,
    )?;
    let error = db
        .create_invitation_request(admin.id, "two@example.com", 300, Some(&changed))
        .await
        .err()
        .unwrap();
    assert_eq!(
        error.downcast_ref::<DomainError>(),
        Some(&DomainError::IdempotencyConflict)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM management_invitations")
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(count, 1);
    let row = sqlx::query("SELECT key_hash,request_hash FROM management_admin_requests")
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(
        row.try_get::<Vec<u8>, _>("key_hash")?,
        Sha256::digest(key.as_bytes()).to_vec()
    );
    let bad = AdminRequest::new(
        "client-request-000002",
        "cancel",
        &serde_json::json!({}),
        false,
    )?;
    assert!(
        db.cancel_invitation_request(admin.id, Uuid::new_v4(), Some(&bad))
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM management_admin_requests")
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(count, 1, "failed effect rolls back receipt too");
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_expired_abandoned_provisioning_and_late_provider_commit_remain_reconcilable()
-> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let invite = db
        .create_invitation(admin.id, "late@example.com", 300)
        .await?;
    let reservation = db.reserve_acceptance(&invite.token, "Late").await?;
    let account = reservation.account_id;
    drop(reservation); // process/request canceled after sending provider create
    sqlx::query("UPDATE management_invitations SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1").bind(invite.invitation.id).execute(&db.pool).await?;
    let job = db
        .next_reconciliation()
        .await?
        .expect("abandoned expiry queues cleanup");
    assert_eq!(job.account_id, account);
    db.reconciliation_complete(&job).await?; // provider absent at first observation
    sqlx::query("UPDATE management_outbox SET completed_at=clock_timestamp()-interval '6 minutes' WHERE id=$1").bind(job.id).execute(&db.pool).await?;
    sqlx::query("UPDATE management_outbox SET available_at=clock_timestamp()+interval '1 hour' WHERE id!=$1").bind(job.id).execute(&db.pool).await?;
    let retry = db
        .next_reconciliation()
        .await?
        .expect("late create remains covered by durable desired state");
    assert_eq!(retry.id, job.id);
    let status: String = sqlx::query_scalar("SELECT status FROM management_accounts WHERE id=$1")
        .bind(account)
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(status, "deleting");
    assert!(db.reserve_acceptance(&invite.token, "Late").await.is_err());
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_provider_lost_create_response_never_resets_retry_password() -> Result<()> {
    use axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        routing::{get, post},
    };
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Provider {
        user: Option<Value>,
        posts: usize,
        first_password_hash: Vec<u8>,
    }
    type Shared = Arc<Mutex<Provider>>;
    async fn lookup(State(state): State<Shared>) -> Json<Value> {
        Json(
            state
                .lock()
                .unwrap()
                .user
                .clone()
                .map(|u| json!([u]))
                .unwrap_or(json!([])),
        )
    }
    async fn create(State(state): State<Shared>, Json(body): Json<Value>) -> StatusCode {
        let mut state = state.lock().unwrap();
        state.posts += 1;
        state.first_password_hash =
            Sha256::digest(body["credentials"][0]["value"].as_str().unwrap().as_bytes()).to_vec();
        state.user = Some(
            json!({"id":"provider-subject","username":body["username"],"attributes":body["attributes"]}),
        );
        StatusCode::BAD_GATEWAY // provider committed, response did not reach caller
    }
    let provider = Shared::default();
    let router = Router::new()
        .route(
            "/realms/test/protocol/openid-connect/token",
            post(|| async { Json(json!({"access_token":"synthetic-admin-token"})) }),
        )
        .route("/admin/realms/test/users", get(lookup).post(create))
        .with_state(provider.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let kc = keycloak::Keycloak::new(
        "https://identity.example/realms/test".into(),
        format!("{base}/admin/realms/test/").parse()?,
        format!("{base}/realms/test/protocol/openid-connect/token").parse()?,
        "fixture".into(),
        "synthetic-secret".into(),
    )?;
    let (db, admin, schema) = fixture().await?;
    let invitation = db
        .create_invitation(admin.id, "ambiguous@example.com", 300)
        .await?;
    let reservation = db.reserve_acceptance(&invitation.token, "Friend").await?;
    let first = kc.provision(&reservation, "first-test-password").await?;
    assert!(
        first.already_existed,
        "ambiguous response requires honest credential warning"
    );
    assert_eq!(first.subject, "provider-subject");
    let second = kc
        .provision(&reservation, "different-test-password")
        .await?;
    assert!(second.already_existed);
    assert_eq!(second.subject, first.subject);
    {
        let state = provider.lock().unwrap();
        assert_eq!(
            state.posts, 1,
            "retry must not submit another password/create"
        );
        assert_eq!(
            state.first_password_hash,
            Sha256::digest(b"first-test-password").to_vec()
        );
    }
    reservation.complete(&second.subject).await?;
    assert!(
        db.reserve_acceptance(&invitation.token, "Again")
            .await
            .is_err()
    );
    server.abort();
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_existing_account_email_requires_deletion_before_new_invitation() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let invitation = db
        .create_invitation(admin.id, "existing@example.com", 300)
        .await?;
    let account = db
        .reserve_acceptance(&invitation.token, "Existing")
        .await?
        .complete("existing-sub")
        .await?;
    for disabled in [false, true] {
        if disabled {
            db.disable_account(admin.id, account.id, false).await?;
        }
        let error = db
            .create_invitation(admin.id, "existing@example.com", 300)
            .await
            .err()
            .expect("existing email cannot issue an unusable invitation");
        assert_eq!(
            error.downcast_ref::<DomainError>(),
            Some(&DomainError::Conflict)
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM management_invitations")
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(count, 1);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM management_audit WHERE action='invite_create'")
            .fetch_one(&db.pool)
            .await?;
    assert_eq!(count, 1);
    db.disable_account(admin.id, account.id, true).await?;
    while let Some(job) = db.next_reconciliation().await? {
        db.reconciliation_complete(&job).await?;
    }
    assert!(
        db.create_invitation(admin.id, "existing@example.com", 300)
            .await
            .is_ok()
    );
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_self_deletion_is_explicit_nonadmin_and_last_admin_safe() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let key = AdminRequest::new(
        "self-delete-admin-0001",
        "self_delete",
        &serde_json::json!({"confirm":true}),
        false,
    )?;
    assert_eq!(
        db.delete_self_request(admin.id, &key)
            .await
            .err()
            .unwrap()
            .downcast_ref::<DomainError>(),
        Some(&DomainError::Forbidden)
    );
    assert!(
        db.active_identity(&db.issuer, "stable-admin-sub")
            .await
            .is_ok()
    );
    let invitation = db
        .create_invitation(admin.id, "self@example.com", 300)
        .await?;
    let account = db
        .reserve_acceptance(&invitation.token, "Self")
        .await?
        .complete("self-sub")
        .await?;
    let key = AdminRequest::new(
        "self-delete-user-00001",
        "self_delete",
        &serde_json::json!({"confirm":true}),
        false,
    )?;
    let deleted = db.delete_self_request(account.id, &key).await?;
    assert_eq!(deleted.status, "deleting");
    assert!(db.active_identity(&db.issuer, "self-sub").await.is_err());
    assert!(
        db.delete_self_request(account.id, &key).await.is_err(),
        "inactive account cannot acquire new authority on replay"
    );
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM management_outbox WHERE account_id=$1")
            .bind(account.id)
            .fetch_one(&db.pool)
            .await?;
    assert_eq!(jobs, 3);
    assert!(
        db.active_identity(&db.issuer, "stable-admin-sub")
            .await
            .is_ok()
    );
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn migrations_empty_and_concurrent_start_create_complete_schema_once() -> Result<()> {
    let (db, schema) = empty_fixture("https://identity.example/realms/test").await?;
    let (first, second) =
        tokio::join!(migrate(&db.pool, MIGRATIONS), migrate(&db.pool, MIGRATIONS));
    first?;
    second?;
    let versions: Vec<i32> =
        sqlx::query_scalar("SELECT version FROM management_schema ORDER BY version")
            .fetch_all(&db.pool)
            .await?;
    assert_eq!(versions, vec![1, 2, 3, 4, 5]);
    for table in [
        "management_accounts",
        "web_sessions",
        "management_device_revocations",
        "management_device_labels",
        "management_waitlist",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&db.pool)
            .await?;
        assert!(exists, "migration did not create {table}");
    }
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn migrations_upgrade_preserves_account_and_browser_state() -> Result<()> {
    for prefix in [1, 2, 3, 4] {
        let (db, schema) = empty_fixture("https://identity.example/realms/test").await?;
        migrate(&db.pool, &MIGRATIONS[..prefix]).await?;
        let account = db.bootstrap("migration-admin", "Existing admin").await?;
        if prefix >= 2 {
            sqlx::query("INSERT INTO web_login_flows(cookie_hash,state_hash,sealed,expires_ms) VALUES($1,$2,$3,9000)")
                .bind(vec![1u8;32]).bind(vec![2u8;32]).bind(b"opaque-existing-ciphertext".to_vec()).execute(&db.pool).await?;
        }
        migrate(&db.pool, MIGRATIONS).await?;
        migrate(&db.pool, MIGRATIONS).await?;
        assert_eq!(
            db.active_identity("https://identity.example/realms/test", "migration-admin")
                .await?
                .id,
            account.id
        );
        if prefix >= 2 {
            let sealed: Vec<u8> = sqlx::query_scalar("SELECT sealed FROM web_login_flows")
                .fetch_one(&db.pool)
                .await?;
            assert_eq!(sealed, b"opaque-existing-ciphertext");
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM management_schema")
            .fetch_one(&db.pool)
            .await?;
        assert_eq!(count, 5);
        cleanup(db, schema).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn migrations_reject_downgrade_and_checksum_change_without_mutating_state() -> Result<()> {
    let (db, schema) = empty_fixture("https://identity.example/realms/test").await?;
    migrate(&db.pool, MIGRATIONS).await?;
    let account = db.bootstrap("migration-admin", "Existing admin").await?;
    let before: Vec<(i32, Vec<u8>)> = sqlx_core::query_as::query_as(
        "SELECT version,checksum FROM management_schema ORDER BY version",
    )
    .fetch_all(&db.pool)
    .await?;
    let error = migrate(&db.pool, &MIGRATIONS[..2]).await.unwrap_err();
    assert!(error.to_string().contains("newer than this binary"));
    let after: Vec<(i32, Vec<u8>)> = sqlx_core::query_as::query_as(
        "SELECT version,checksum FROM management_schema ORDER BY version",
    )
    .fetch_all(&db.pool)
    .await?;
    assert_eq!(before, after);
    assert_eq!(
        db.active_identity("https://identity.example/realms/test", "migration-admin")
            .await?
            .id,
        account.id
    );
    cleanup(db, schema).await?;

    let (db, schema) = empty_fixture("https://identity.example/realms/test").await?;
    migrate(&db.pool, &MIGRATIONS[..2]).await?;
    sqlx::query("UPDATE management_schema SET checksum=$1 WHERE version=1")
        .bind(vec![0u8; 32])
        .execute(&db.pool)
        .await?;
    let error = migrate(&db.pool, MIGRATIONS).await.unwrap_err();
    assert!(error.to_string().contains("checksum mismatch"));
    let third_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('management_device_revocations') IS NOT NULL")
            .fetch_one(&db.pool)
            .await?;
    assert!(
        !third_exists,
        "checksum rejection must happen before new DDL"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM management_schema")
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(count, 2);
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_reconciliation_lookup_requires_unaccepted_invitation_without_any_identity()
-> Result<()> {
    let (db, admin, schema) = fixture().await?;
    for state in ["ambiguous", "accepted_missing", "other_issuer"] {
        let invite = db
            .create_invitation(admin.id, &format!("{state}@example.invalid"), 300)
            .await?;
        let acceptance = db.reserve_acceptance(&invite.token, "Fixture").await?;
        let id = acceptance.account_id;
        if state == "ambiguous" {
            drop(acceptance);
            db.cancel_invitation(admin.id, invite.invitation.id).await?;
        } else {
            acceptance.complete(&format!("subject-{state}")).await?;
            if state == "accepted_missing" {
                sqlx::query("DELETE FROM management_identities WHERE account_id=$1")
                    .bind(id)
                    .execute(&db.pool)
                    .await?;
            } else {
                sqlx::query("UPDATE management_identities SET issuer='https://other.example/realms/test' WHERE account_id=$1").bind(id).execute(&db.pool).await?;
            }
            db.disable_account(admin.id, id, true).await?;
        }
        let mut count = 0;
        while let Some(job) = db.next_reconciliation().await? {
            assert_eq!(job.account_id, id);
            assert_eq!(job.issuer, db.issuer);
            assert!(job.subject.is_none());
            assert_eq!(job.allow_provisioning_lookup, state == "ambiguous");
            assert!(job.allow_provisioned_cleanup);
            count += 1;
            db.reconciliation_complete(&job).await?;
        }
        assert_eq!(count, 3);
    }
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_device_labels_cas_replay_revocation_and_persistence() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let invitation = db
        .create_invitation(admin.id, "labels-other@example.invalid", 300)
        .await?;
    let other = db
        .reserve_acceptance(&invitation.token, "Other")
        .await?
        .complete("other-label-sub")
        .await?;
    let request = |key: &str, label: Option<&str>, revision: i64| {
        AdminRequest::new(
            key,
            "rename_device",
            &serde_json::json!({"label":label,"expected_revision":revision}),
            false,
        )
    };
    let first = request("label-request-first-0001", Some("  Cafe\u{301}  "), 0)?;
    let result = db
        .rename_device_request(
            admin.id,
            "mac",
            "mac-a",
            "mac-a",
            Some("  Cafe\u{301}  "),
            0,
            &first,
            || Ok(true),
        )
        .await?;
    assert_eq!(result.label.as_deref(), Some("Café"));
    assert_eq!(result.label_revision, 1);
    let replay = db
        .rename_device_request(
            admin.id,
            "mac",
            "mac-a",
            "mac-a",
            Some("  Cafe\u{301}  "),
            0,
            &first,
            || Ok(true),
        )
        .await
        .unwrap_err();
    assert_eq!(
        replay.downcast_ref::<DomainError>(),
        Some(&DomainError::AlreadyApplied)
    );
    let changed = request("label-request-first-0001", Some("Different"), 1)?;
    assert_eq!(
        db.rename_device_request(
            admin.id,
            "mac",
            "mac-a",
            "mac-a",
            Some("Different"),
            1,
            &changed,
            || Ok(true)
        )
        .await
        .unwrap_err()
        .downcast_ref::<DomainError>(),
        Some(&DomainError::IdempotencyConflict)
    );
    let a = request("label-concurrent-request-a", Some("A"), 1)?;
    let b = request("label-concurrent-request-b", Some("B"), 1)?;
    let (a, b) = tokio::join!(
        db.rename_device_request(admin.id, "mac", "mac-a", "mac-a", Some("A"), 1, &a, || Ok(
            true
        )),
        db.rename_device_request(admin.id, "mac", "mac-a", "mac-a", Some("B"), 1, &b, || Ok(
            true
        ))
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "two concurrent editors must not both win"
    );
    let failure = if let Err(error) = a {
        error
    } else {
        b.unwrap_err()
    };
    assert_eq!(
        failure.downcast_ref::<DomainError>(),
        Some(&DomainError::Conflict)
    );
    let clear = request("label-clear-request-00001", None, 2)?;
    db.rename_device_request(admin.id, "mac", "mac-a", "mac-a", None, 2, &clear, || {
        Ok(true)
    })
    .await?;
    // New management handle reads committed state; clearing never resets the CAS revision.
    let persisted = db.clone().device_labels(admin.id).await?;
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].label, None);
    assert_eq!(persisted[0].label_revision, 3);
    assert!(db.device_labels(other.id).await?.is_empty());
    let foreign = request("label-foreign-request-001", Some("Foreign"), 0)?;
    assert_eq!(
        db.rename_device_request(
            other.id,
            "mac",
            "mac-a",
            "mac-a",
            Some("Foreign"),
            0,
            &foreign,
            || Ok(false)
        )
        .await
        .unwrap_err()
        .downcast_ref::<DomainError>(),
        Some(&DomainError::NotFound)
    );
    let revoke = AdminRequest::new(
        "label-revoke-request-0001",
        "revoke_device",
        &serde_json::json!({}),
        false,
    )?;
    db.revoke_device_request(admin.id, "mac", "mac-a", "mac-a", &[], &revoke)
        .await?;
    let after = request("label-after-revoke-00001", Some("Forbidden"), 3)?;
    assert_eq!(
        db.rename_device_request(
            admin.id,
            "mac",
            "mac-a",
            "mac-a",
            Some("Forbidden"),
            3,
            &after,
            || Ok(true)
        )
        .await
        .unwrap_err()
        .downcast_ref::<DomainError>(),
        Some(&DomainError::NotFound)
    );
    // Alias text is account metadata, never copied into audit logs or idempotency records.
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM management_audit WHERE action='device_label'")
            .fetch_all(&db.pool)
            .await?;
    assert_eq!(actions.len(), 3);
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn management_device_labels_delete_is_atomic_and_account_scoped() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let invite = db
        .create_invitation(admin.id, "delete-label@example.invalid", 300)
        .await?;
    let other = db
        .reserve_acceptance(&invite.token, "Other")
        .await?
        .complete("delete-label-sub")
        .await?;
    for (actor, id) in [(admin.id, "admin-mac"), (other.id, "other-mac")] {
        let request = AdminRequest::new(
            "label-deletion-setup-0001",
            "rename_device",
            &serde_json::json!({}),
            false,
        )?;
        db.rename_device_request(
            actor,
            "mac",
            id,
            id,
            Some("Private alias"),
            0,
            &request,
            || Ok(true),
        )
        .await?;
    }
    db.disable_account(admin.id, other.id, false).await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM management_device_labels WHERE account_id=$1"
        )
        .bind(other.id)
        .fetch_one(&db.pool)
        .await?,
        1,
        "temporary disable preserves alias"
    );
    // Force a later transaction statement to fail: deletion must not partially
    // erase aliases while leaving the account transition uncommitted.
    sqlx::raw_sql("ALTER TABLE management_audit ADD CONSTRAINT synthetic_delete_failure CHECK(action != 'account_delete') NOT VALID").execute(&db.pool).await?;
    assert!(db.disable_account(admin.id, other.id, true).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM management_device_labels")
            .fetch_one(&db.pool)
            .await?,
        2
    );
    sqlx::raw_sql("ALTER TABLE management_audit DROP CONSTRAINT synthetic_delete_failure")
        .execute(&db.pool)
        .await?;
    db.disable_account(admin.id, other.id, true).await?;
    db.disable_account(admin.id, other.id, true).await?;
    let remaining: Vec<Uuid> =
        sqlx::query_scalar("SELECT account_id FROM management_device_labels")
            .fetch_all(&db.pool)
            .await?;
    assert_eq!(remaining, vec![admin.id]);
    cleanup(db, schema).await
}

#[tokio::test]
#[ignore = "requires disposable local PostgreSQL"]
async fn waitlist_deduplicates_by_normalized_email_in_the_database() -> Result<()> {
    let (db, admin, schema) = fixture().await?;
    let email = waitlist::normalize(" Early.Bird@Example.COM ").unwrap();
    let (first, second) = tokio::join!(db.join_waitlist(&email), db.join_waitlist(&email));
    first?;
    second?;
    db.join_waitlist(&email).await?;
    assert!(db.join_waitlist("Early.Bird@Example.COM").await.is_err());
    let duplicate = sqlx::query("INSERT INTO management_waitlist(id,email) VALUES($1,$2)")
        .bind(Uuid::new_v4())
        .bind(&email)
        .execute(&db.pool)
        .await
        .unwrap_err();
    assert_eq!(
        duplicate.as_database_error().and_then(|e| e.constraint()),
        Some("management_waitlist_email_key")
    );
    assert!(
        sqlx::query("INSERT INTO management_waitlist(id,email) VALUES($1,'Upper@Example.com')")
            .bind(Uuid::new_v4())
            .execute(&db.pool)
            .await
            .is_err(),
        "the table itself refuses unnormalized addresses"
    );
    let page = db.list_waitlist(admin.id, None, 10).await?;
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].email, "early.bird@example.com");
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM management_accounts")
        .fetch_one(&db.pool)
        .await?;
    let invitations: i64 = sqlx::query_scalar("SELECT count(*) FROM management_invitations")
        .fetch_one(&db.pool)
        .await?;
    assert_eq!((accounts, invitations), (1, 0), "joining grants nothing");
    cleanup(db, schema).await
}
