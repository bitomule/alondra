use super::*;

// Run only against a disposable local PostgreSQL database. Each test gets its own
// schema; concurrency assertions exercise PostgreSQL locks, not SQLite substitutes.
async fn fixture() -> Result<(Management, Account, String)> {
    fixture_for_issuer("https://identity.example/realms/test").await
}
pub(crate) async fn fixture_for_issuer(issuer: &str) -> Result<(Management, Account, String)> {
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
    sqlx::raw_sql(include_str!("../../migrations/0001_management.sql"))
        .execute(&pool)
        .await?;
    sqlx::raw_sql(include_str!("../../migrations/0003_device_revocations.sql"))
        .execute(&pool)
        .await?;
    admin.close().await;
    let management = Management {
        pool,
        issuer: issuer.into(),
    };
    let actor = management.bootstrap("stable-admin-sub", "Admin").await?;
    Ok((management, actor, schema))
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
            "/token",
            post(|| async { Json(json!({"access_token":"synthetic-admin-token"})) }),
        )
        .route("/admin/realms/test/users", get(lookup).post(create))
        .with_state(provider.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let kc = keycloak::Keycloak::new(
        format!("{base}/admin/realms/test/").parse()?,
        format!("{base}/token").parse()?,
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
