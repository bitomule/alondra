// Opt-in mechanism tests, not attribution of an earlier load failure. A real local
// PostgreSQL is required; the HTTP fixture uses real auth/crypto/queue, but does
// not run the background management worker or contact Keycloak.
mod pg_fault_tests {
    use super::*;
    use anyhow::{Context as _, Result, ensure};
    use sqlx_postgres::PgPoolOptions;
    mod sqlx {
        pub use sqlx_core::{error::Error, query::query, query_scalar::query_scalar};
    }
    use std::time::Duration;

    // Run deliberately, never as a default gate:
    // ALONDRA_MANAGEMENT_TEST_DATABASE_URL=<disposable loopback DB> cargo test
    //   -p alondra-relay --bin alondra-relay http_tests::pg_fault_tests:: -- --ignored
    // These tests prove two possible mechanisms and their recovery, not the
    // cause of the historical tuned-load failure (its SQL error was not saved).
    const ISSUER: &str = "https://id.example/realms/alondra";
    struct AbortServer(tokio::task::JoinHandle<()>);
    impl Drop for AbortServer {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    // Keep the ordinary six-connection fixture unchanged. Replace only this
    // fixture's pool with the product's explicit 8 / 3s / 5s configuration.
    async fn run_case(statement: bool) -> Result<()> {
        let (mut db, admin, schema) = management::tests::fixture_for_issuer(ISSUER).await?;
        db.pool.close().await;
        let search_path = schema.clone();
        let pool = PgPoolOptions::new().max_connections(8)
            .acquire_timeout(Duration::from_secs(3))
            .after_connect(move |conn, _| {
                let schema = search_path.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path',$1,false), set_config('statement_timeout','5s',false)")
                        .bind(schema).execute(conn).await?;
                    Ok(())
                })
            }).connect(&std::env::var("ALONDRA_MANAGEMENT_TEST_DATABASE_URL")?).await;
        let result = match pool {
            Ok(pool) => {
                db.pool = pool;
                let inner = db.clone();
                // JoinError captures assertion panics. The outer owner always
                // closes connections and drops its isolated schema afterwards.
                let task = tokio::spawn(async move {
                    tokio::time::timeout(
                        Duration::from_secs(30),
                        exercise(inner, admin.id, statement),
                    )
                    .await?
                });
                match task.await {
                    Ok(result) => result,
                    Err(error) => Err(error.into()),
                }
            }
            Err(error) => Err(error.into()),
        };
        let cleanup = management::tests::cleanup(db, schema).await;
        result?;
        cleanup
    }

    #[tokio::test]
    #[ignore = "requires disposable local PostgreSQL; explicit pool exhaustion and recovery"]
    async fn pg_pool_timeout_http_503_and_encrypted_recovery() -> Result<()> {
        run_case(false).await
    }

    #[tokio::test]
    #[ignore = "requires disposable local PostgreSQL; explicit table lock and recovery"]
    async fn pg_statement_timeout_http_503_and_encrypted_recovery() -> Result<()> {
        run_case(true).await
    }

    async fn exercise(
        db: management::Management,
        admin: uuid::Uuid,
        statement: bool,
    ) -> Result<()> {
        let invite = db
            .create_invitation(admin, "fault-owner@example.com", 300)
            .await?;
        db.reserve_acceptance(&invite.token, "Owner")
            .await?
            .complete("owner")
            .await?;
        let mac = SigningKey::from_slice(&[21; 32])?;
        let phone = SigningKey::from_slice(&[22; 32])?;
        let mac_ecdh = SecretKey::from_slice(&[23; 32])?;
        let phone_ecdh = SecretKey::from_slice(&[24; 32])?;
        let (url, server, join) = test_server_with_management(&mac, None, Some(db.clone())).await;
        let _server_lifetime = AbortServer(join);
        server.auth.enroll(
            &auth::Identity {
                device_id: "mac-1".into(),
                owner_sub: "owner".into(),
            },
            PhoneDevice {
                device_id: "phone-1".into(),
                owner_sub: "owner".into(),
                signing_key: key_bytes(&phone),
            },
        )?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()?;
        let epoch = server.auth.server_epoch();
        let envelope = Envelope::seal_random(
            Context {
                route: "mac-1",
                sender: "phone-1",
                recipient: "mac-1",
                request_id: "pg-fault",
                kind: "request",
                expires_at_ms: now_ms() + 120_000,
            },
            b"private request",
            &phone,
            &mac_ecdh.public_key(),
        )?;
        let body = serde_json::to_vec(&envelope)?;
        let request = |is_phone: bool, method: &str, path: &str, body: Vec<u8>, nonce: u8| {
            let mut builder = client
                .request(
                    method.parse::<reqwest::Method>().unwrap(),
                    format!("{url}{path}"),
                )
                .body(body.clone());
            if is_phone {
                builder = builder.bearer_auth(jwt("owner", now_ms()));
            }
            sign(
                &epoch,
                builder,
                if is_phone { &phone } else { &mac },
                if is_phone { "phone-1" } else { "mac-1" },
                method,
                path,
                &body,
                nonce,
            )
        };
        assert_eq!(
            client
                .get(format!("{url}/v1/macs"))
                .bearer_auth(jwt("owner", now_ms()))
                .send()
                .await?
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(false, "GET", "/v1/work?wait_s=0", vec![], 1)
                .send()
                .await?
                .status(),
            StatusCode::NO_CONTENT
        );

        let mut leases = Vec::new();
        let mut lock = None;
        let mut control_pool = None;
        if statement {
            // This pool is independent of the product's eight slots. An UPDATE
            // row lock would not block the ordinary MVCC authorization SELECT.
            let control = PgPoolOptions::new()
                .max_connections(1)
                .connect(&std::env::var("ALONDRA_MANAGEMENT_TEST_DATABASE_URL")?)
                .await?;
            let mut tx = control.begin().await?;
            let schema: String = sqlx::query_scalar("SELECT current_schema()")
                .fetch_one(&db.pool)
                .await?;
            ensure!(
                schema.starts_with("management_test_")
                    && schema
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
            );
            sqlx::query(&format!(
                "LOCK TABLE {schema}.management_accounts IN ACCESS EXCLUSIVE MODE"
            ))
            .execute(&mut *tx)
            .await?;
            lock = Some(tx); // acquisition completed: deterministic fault barrier
            control_pool = Some(control);
        } else {
            for _ in 0..8 {
                leases.push(db.pool.acquire().await?);
            }
            assert_eq!(db.pool.size(), 8);
            assert_eq!(db.pool.num_idle(), 0); // all eight acquired before requests
        }
        let (domain, http) = tokio::join!(
            db.require_live_device("owner", "mac-1", Some("phone-1")),
            request(true, "POST", "/v1/requests", body.clone(), 2).send()
        );
        let error = domain.expect_err("fault must reach the database authorization query");
        let sql = error
            .downcast_ref::<sqlx::Error>()
            .context("expected typed SQLx failure")?;
        if statement {
            assert!(
                db.pool.size() < 8,
                "statement case must not exhaust the pool"
            );
            assert_eq!(
                sql.as_database_error().and_then(|e| e.code()).as_deref(),
                Some("57014")
            );
        } else {
            assert!(matches!(sql, sqlx::Error::PoolTimedOut));
        }
        let http = http?;
        assert_eq!(http.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            http.json::<Value>().await?["error"],
            "account_access_unavailable"
        );
        assert!(
            matches!(
                server.queue.get("owner", "phone-1", "pg-fault", now_ms()),
                Err(state::QueueError::NotFound)
            ),
            "503 must precede queue admission"
        );
        if let Some(tx) = lock {
            tx.rollback().await?;
        }
        drop(leases);
        if let Some(pool) = control_pool {
            pool.close().await;
        }

        assert_eq!(
            request(true, "GET", "/v1/requests/pg-fault", vec![], 3)
                .send()
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );
        let accepted = request(true, "POST", "/v1/requests", body.clone(), 4)
            .send()
            .await?;
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        let accepted: Value = accepted.json().await?;
        let duplicate = request(true, "POST", "/v1/requests", body, 5)
            .send()
            .await?;
        assert_eq!(duplicate.status(), StatusCode::ACCEPTED);
        assert_eq!(
            duplicate.json::<Value>().await?,
            accepted,
            "same encrypted request must keep its original queue revision"
        );
        let work = request(false, "GET", "/v1/work?wait_s=0", vec![], 6)
            .send()
            .await?;
        assert_eq!(work.status(), StatusCode::OK);
        let work: Value = work.json().await?;
        let forwarded: Envelope = serde_json::from_value(work["envelope"].clone())?;
        assert_eq!(
            forwarded.open(
                phone.verifying_key(),
                &mac_ecdh,
                "phone-1",
                "mac-1",
                "mac-1",
                now_ms()
            )?,
            b"private request"
        );
        assert_eq!(
            request(false, "POST", "/v1/work/pg-fault/receipt", vec![], 7)
                .send()
                .await?
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(false, "GET", "/v1/work?wait_s=0", vec![], 8)
                .send()
                .await?
                .status(),
            StatusCode::NO_CONTENT,
            "duplicate did not enqueue another work item"
        );
        let response = Envelope::seal_random(
            Context {
                route: "mac-1",
                sender: "mac-1",
                recipient: "phone-1",
                request_id: "pg-fault",
                kind: "response",
                expires_at_ms: now_ms() + 120_000,
            },
            b"private result",
            &mac,
            &phone_ecdh.public_key(),
        )?;
        assert_eq!(
            request(
                false,
                "POST",
                "/v1/work/pg-fault/response",
                serde_json::to_vec(&response)?,
                9
            )
            .send()
            .await?
            .status(),
            StatusCode::OK
        );
        let result = request(true, "GET", "/v1/requests/pg-fault", vec![], 10)
            .send()
            .await?;
        assert_eq!(result.status(), StatusCode::OK);
        let result: Value = result.json().await?;
        assert_eq!(result["status"], "completed");
        let result: Envelope = serde_json::from_value(result["response"].clone())?;
        assert_eq!(
            result.open(
                mac.verifying_key(),
                &phone_ecdh,
                "mac-1",
                "phone-1",
                "mac-1",
                now_ms()
            )?,
            b"private result"
        );
        assert_eq!(
            client
                .get(format!("{url}/v1/macs"))
                .bearer_auth(jwt("stable-admin-sub", now_ms()))
                .send()
                .await?
                .status(),
            StatusCode::OK,
            "another account recovers too"
        );
        let mut recovered = Vec::new();
        for _ in 0..8 {
            recovered.push(db.pool.acquire().await?);
        }
        drop(recovered);
        assert_eq!(server.work_waiters.available_permits(), 16);
        assert_eq!(server.change_waiters.available_permits(), 64);
        assert_eq!(server.requests.available_permits(), 128);
        Ok(())
    }
}
