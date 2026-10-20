// Opt-in mechanism tests, not attribution of an earlier load failure. A real local
// PostgreSQL is required; the HTTP fixture uses real auth and crypto, but does
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
    async fn pg_pool_timeout_http_503_and_recovery() -> Result<()> {
        run_case(false).await
    }

    #[tokio::test]
    #[ignore = "requires disposable local PostgreSQL; explicit table lock and recovery"]
    async fn pg_statement_timeout_http_503_and_recovery() -> Result<()> {
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
        let path = "/v1/iroh-node";
        let request = |is_phone: bool, nonce: u8| {
            let mut builder = client.delete(format!("{url}{path}"));
            if is_phone {
                builder = builder.bearer_auth(jwt("owner", now_ms()));
            }
            sign(
                &epoch,
                builder,
                if is_phone { &phone } else { &mac },
                if is_phone { "phone-1" } else { "mac-1" },
                "DELETE",
                path,
                &[],
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
            request(false, 1).send().await?.status(),
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
            request(true, 2).send()
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
        if let Some(tx) = lock {
            tx.rollback().await?;
        }
        drop(leases);
        if let Some(pool) = control_pool {
            pool.close().await;
        }

        assert_eq!(request(true, 3).send().await?.status(), StatusCode::NO_CONTENT);
        assert_eq!(request(false, 4).send().await?.status(), StatusCode::NO_CONTENT);
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
        assert_eq!(server.requests.available_permits(), 128);
        Ok(())
    }
}
