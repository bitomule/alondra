use super::actions::{ActionRequest, execute};
use crate::auth::Signed;
use crate::domain_runtime::DomainRuntime;
use anyhow::{Context, Result};
use axum::{Extension, Json, extract::State, http::{StatusCode, Uri}, response::{IntoResponse, Response}};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, sync::Arc};

fn hexadecimal(bytes: &[u8]) -> String { bytes.iter().map(|byte|format!("{byte:02x}")).collect() }

fn actor(home: &Path, signed: &Signed) -> Result<String> {
    let auth = crate::auth::Auth::open(home)?;
    let approval = auth.list().into_iter().find(|device|
        device.id == signed.device_id && device.public_key == signed.public_key).context("device approval was revoked")?;
    Ok(hexadecimal(&Sha256::digest(serde_json::to_vec(&("actor_loopback", &signed.device_id,
        &signed.public_key, approval.created_at))?)))
}

const JOURNAL_MARKER: &[u8] = b"alondra-local-action-results\n";

fn open(home: &Path) -> Result<Connection> {
    use std::io::{Read, Write};
    let path = home.join("action-results.sqlite");
    let marker = home.join("action-results.schema");
    let initialized = marker.try_exists()?;
    if initialized {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        let file = options.open(&marker)?;
        anyhow::ensure!(file.metadata()?.is_file(), "action journal marker must be a regular file");
        let mut bytes = Vec::new();
        file.take(128).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes==JOURNAL_MARKER,"invalid action journal marker");
        anyhow::ensure!(path.try_exists()?,"initialized action journal is missing");
        anyhow::ensure!(std::fs::symlink_metadata(&path)?.is_file(),"action journal must be a regular file");
        anyhow::ensure!(std::fs::metadata(&path)?.len()>0,"initialized action journal is empty");
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(!initialized).read(true).write(true).truncate(false);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(&path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "action journal must be a regular file");
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
    let table: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='action_results')",[],|row|row.get(0))?;
    if initialized { anyhow::ensure!(table,"initialized action journal has no receipt table"); }
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    if !initialized {
        if table {
            let rows: i64 = c.query_row("SELECT count(*) FROM action_results",[],|row|row.get(0))?;
            anyhow::ensure!(rows==0,"action journal lost its initialized marker");
        }
        c.execute_batch("CREATE TABLE IF NOT EXISTS action_results(actor TEXT NOT NULL, command_id TEXT NOT NULL,
            data_epoch TEXT NOT NULL, digest BLOB NOT NULL, state TEXT NOT NULL,
            response BLOB, PRIMARY KEY(actor,command_id,data_epoch));")?;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(&marker)?;
        file.write_all(JOURNAL_MARKER)?;
        file.sync_all()?;
        std::fs::File::open(home)?.sync_all()?;
    }
    Ok(c)
}

async fn journal<T: Send+'static>(domain: &DomainRuntime, home: PathBuf, signed: Signed,
    operation: impl FnOnce(&mut Connection, &str)->Result<T> +Send+'static) -> Result<T> {
    let admitted = domain.local_actions.journal_admissions.clone().try_acquire_owned()
        .context("action result journal admission full")?;
    let physical = domain.local_actions.journal_worker.clone().acquire_owned().await?;
    tokio::task::spawn_blocking(move || {
        let (_admitted, _physical) = (admitted, physical);
        let _approval = crate::approval_lease::ApprovalLease::shared(&home)?;
        let actor = actor(&home, &signed)?;
        operation(&mut open(&home)?, &actor)
    }).await?
}

enum Admission { Execute, Completed(Vec<u8>), Unknown }

pub(super) async fn execute_recorded(
    domain: &DomainRuntime, client: &crate::client_routes::Client, home: &Path, signed: &Signed,
    raw_body: &[u8], request: ActionRequest, authorize: impl Fn()->Result<()>+Sync,
) -> Response {
    if !super::actions::is_uuid_v4(&request.command_id) { return unknown(StatusCode::UNPROCESSABLE_ENTITY); }
    let epoch = request.data_epoch.clone();
    if !domain.state().is_ok_and(|state|state.data_epoch.0==epoch) { return unknown(StatusCode::CONFLICT); }
    let Ok(original_actor) = actor(home,signed) else { return unknown(StatusCode::FORBIDDEN); };
    let digest: [u8;32] = Sha256::digest(raw_body).into();
    let (id, expected_epoch) = (request.command_id.clone(), epoch.clone());
    let admitted = journal(domain, home.to_owned(), signed.clone(), move |c, actor| {
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let prior: Option<(Vec<u8>,String,Option<Vec<u8>>)> = tx.query_row(
            "SELECT digest,state,response FROM action_results WHERE actor=?1 AND command_id=?2 AND data_epoch=?3",
            params![actor,id,expected_epoch], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        let decision = match prior {
            Some((previous,state,Some(body))) if previous==digest && state=="completed" => Admission::Completed(body),
            Some(_) => Admission::Unknown,
            None => {
                tx.execute("INSERT INTO action_results(actor,command_id,data_epoch,digest,state) VALUES(?1,?2,?3,?4,'executing')",
                    params![actor,id,expected_epoch,digest.as_slice()])?;
                Admission::Execute
            },
        };
        tx.commit()?;
        Ok(decision)
    }).await;
    match admitted {
        Ok(Admission::Completed(bytes)) => {
            if authorize().is_err() || actor(home,signed).ok().as_ref()!=Some(&original_actor)
                || !domain.state().is_ok_and(|state|state.data_epoch.0==epoch) { return unknown(StatusCode::FORBIDDEN); }
            return ([(axum::http::header::CONTENT_TYPE,"application/json")],bytes).into_response();
        },
        Ok(Admission::Execute) => {},
        Ok(Admission::Unknown) => return unknown(StatusCode::CONFLICT),
        Err(_) => return unknown(StatusCode::SERVICE_UNAVAILABLE),
    }
    if authorize().is_err() || actor(home,signed).ok().as_ref()!=Some(&original_actor)
        || !domain.state().is_ok_and(|state|state.data_epoch.0==epoch) { return unknown(StatusCode::FORBIDDEN); }
    let response = execute(domain,client,home,&signed.device_id,request.clone(),None,&authorize).await;
    let status = response.status();
    let Ok(bytes) = axum::body::to_bytes(response.into_body(),crate::service::MAX_LOCAL_RESPONSE).await else {
        return unknown(StatusCode::INTERNAL_SERVER_ERROR);
    };
    if status!=StatusCode::OK { return (status,bytes).into_response(); }
    let saved = bytes.to_vec();
    let id = request.command_id;
    let expected_epoch = epoch.clone();
    let result = journal(domain,home.to_owned(),signed.clone(),move |c, actor| {
        let changed = c.execute("UPDATE action_results SET state='completed',response=?5 WHERE actor=?1 AND command_id=?2 AND data_epoch=?3 AND digest=?4 AND state='executing'",
            params![actor,id,epoch,digest.as_slice(),saved])?;
        anyhow::ensure!(changed==1,"action receipt changed before completion");
        Ok(())
    }).await;
    if result.is_err() || authorize().is_err() || actor(home,signed).ok().as_ref()!=Some(&original_actor)
        || !domain.state().is_ok_and(|state|state.data_epoch.0==expected_epoch) { return unknown(StatusCode::SERVICE_UNAVAILABLE); }
    ([(axum::http::header::CONTENT_TYPE,"application/json")],bytes).into_response()
}

fn unknown(status: StatusCode) -> Response { (status,Json(json!({"error":"outcome_unknown"}))).into_response() }

pub(super) async fn read(
    State(state): State<Arc<crate::domain_bootstrap::DomainDaemon>>,
    signed: Option<Extension<Signed>>, uri: Uri,
) -> Response {
    let home = crate::domain_api::DomainHost::home(&*state).to_owned();
    let Some(device) = crate::domain_bootstrap::signed_device(&home,&signed) else { return unknown(StatusCode::FORBIDDEN); };
    let target = uri.path_and_query().map_or(uri.path(),|value|value.as_str());
    let Some(lookup) = crate::service::operation_results::lookup(target) else { return unknown(StatusCode::UNPROCESSABLE_ENTITY); };
    if lookup.offset.is_some() { return unknown(StatusCode::UNPROCESSABLE_ENTITY); }
    let domain = state.domain();
    let Ok(original_actor) = actor(&home,&device) else { return unknown(StatusCode::FORBIDDEN); };
    if !domain.state().is_ok_and(|current|current.data_epoch.0==lookup.data_epoch) { return unknown(StatusCode::CONFLICT); }
    let (id,epoch,digest) = (lookup.id.clone(),lookup.data_epoch.clone(),lookup.digest);
    let result = journal(&domain,home.clone(),device.clone(),move |c,actor| {
        let result: Option<Vec<u8>> = c.query_row("SELECT response FROM action_results WHERE actor=?1 AND command_id=?2 AND data_epoch=?3 AND digest=?4 AND state='completed'",
            params![actor,id,epoch,digest.as_slice()],|row|row.get(0)).optional()?;
        Ok(result)
    }).await;
    if crate::domain_bootstrap::signed_device(&home,&signed).is_none()
        || actor(&home,&device).ok().as_ref()!=Some(&original_actor) { return unknown(StatusCode::FORBIDDEN); }
    if !domain.state().is_ok_and(|current|current.data_epoch.0==lookup.data_epoch) { return unknown(StatusCode::CONFLICT); }
    let Ok(Some(bytes)) = result else { return unknown(StatusCode::NOT_FOUND); };
    let Ok(response) = serde_json::from_slice::<Value>(&bytes) else { return unknown(StatusCode::INTERNAL_SERVER_ERROR); };
    Json(json!({"request_id":lookup.id,"digest":lookup.digest_text,"data_epoch":lookup.data_epoch,"response":response})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _,engine::general_purpose::STANDARD};
    use p256::ecdsa::{Signature,SigningKey,signature::Signer};
    use std::future::IntoFuture;
    use std::sync::atomic::{AtomicI64,Ordering};
    use alondra_core::views::action_id::{ActionId,Verb};

    struct Peer {
        key: SigningKey,
        device: String,
        last: AtomicI64,
        client: reqwest::Client,
    }
    impl Peer {
        fn new(home: &Path) -> Self {
            let key = SigningKey::from_slice(&[41u8;32]).unwrap();
            let auth = crate::auth::Auth::open(home).unwrap();
            let device = auth.pair(&auth.new_pairing_code(),"Fixture Mac",&STANDARD.encode(key.verifying_key().to_sec1_bytes())).unwrap();
            Self { key,device:device.id,last:AtomicI64::new(0),client:reqwest::Client::builder().pool_max_idle_per_host(0).build().unwrap() }
        }
        async fn request(&self,base:&str,method:&str,path:&str,body:&[u8])->(StatusCode,Vec<u8>) {
            let now = alondra_core::now_millis();
            let timestamp = self.last.fetch_update(Ordering::SeqCst,Ordering::SeqCst,|old|Some(now.max(old+1))).unwrap();
            let timestamp = now.max(timestamp+1);
            let signature: Signature = self.key.sign(crate::auth::signed_message(method,path,&timestamp.to_string(),body).as_bytes());
            let reply = self.client.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(),format!("{base}{path}"))
                .header("x-alondra-device",&self.device).header("x-alondra-timestamp",timestamp.to_string())
                .header("x-alondra-signature",STANDARD.encode(signature.to_der().as_bytes()))
                .header("content-type","application/json").body(body.to_vec()).send().await.unwrap();
            (reply.status(),reply.bytes().await.unwrap().to_vec())
        }
    }
    struct Server {
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
        task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    }
    impl Server {
        async fn stop(mut self) {
            if let Some(shutdown) = self.shutdown.take() { let _ = shutdown.send(()); }
            self.task.take().unwrap().await.unwrap().unwrap();
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() { let _ = shutdown.send(()); }
        }
    }
    async fn serve(home:Arc<crate::test_support::Directory>)->(Arc<crate::domain_bootstrap::DomainDaemon>,String,Server) {
        drop(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let state = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
        let router = crate::domain_bootstrap::router(state.clone())
            .layer(axum::middleware::from_fn_with_state(state.auth(),crate::auth::guard))
            .layer(axum::Extension(home));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}",listener.local_addr().unwrap());
        let (shutdown,closed) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(axum::serve(listener,router.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .with_graceful_shutdown(async { let _ = closed.await; }).into_future());
        (state,url,Server { shutdown:Some(shutdown),task:Some(task) })
    }
    fn action(state:&crate::domain_bootstrap::DomainDaemon)->(String,Vec<u8>,String) {
        let current = state.domain().state().unwrap();
        let id = "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10".to_owned();
        let body = serde_json::to_vec(&json!({"protocol":1,"command_id":id,"data_epoch":current.data_epoch,
            "client":"mac","lang":"en","action_id":ActionId::new(Verb::SetLimit,"limit",current.revision,Some("4".into())).unwrap().to_string(),
            "option_id":"4","input":{}})).unwrap();
        let path = format!("/operation-results/{id}?digest={}&data_epoch={}",hexadecimal(&Sha256::digest(&body)),current.data_epoch.0);
        (id,body,path)
    }

    #[tokio::test]
    async fn a_signed_local_get_recovers_the_exact_action_reply_after_restart_without_reapplying_it() {
        let home = Arc::new(crate::test_support::Directory::new());
        std::fs::create_dir_all(&**home).unwrap();
        let peer = Peer::new(&home);
        let (state,base,server) = serve(home.clone()).await;
        let (_,body,path) = action(&state);
        let (status,response) = peer.request(&base,"POST","/actions",&body).await;
        assert_eq!(status,StatusCode::OK);
        assert_eq!(serde_json::from_slice::<Value>(&response).unwrap()["outcome"],"applied");
        let options = alondra_core::options::path_in(&home);
        let changed = std::fs::read_to_string(&options).unwrap().replace("max_working_nodes = 4","max_working_nodes = 5");
        std::fs::write(&options,&changed).unwrap();
        server.stop().await; drop(state);
        let (restarted,base,server) = serve(home.clone()).await;
        let (status,wrapper) = peer.request(&base,"GET",&path,&[]).await;
        assert_eq!(status,StatusCode::OK);
        let wrapper: Value = serde_json::from_slice(&wrapper).unwrap();
        assert_eq!(wrapper["response"],serde_json::from_slice::<Value>(&response).unwrap());
        assert_eq!(std::fs::read_to_string(&options).unwrap(),changed);
        let (status,repeated) = peer.request(&base,"POST","/actions",&body).await;
        assert_eq!(status,StatusCode::OK);
        assert_eq!(repeated,response);
        assert_eq!(std::fs::read_to_string(&options).unwrap(),changed);
        let mut whitespace = body.clone(); whitespace.push(b' ');
        let bad_path = path.replace(&hexadecimal(&Sha256::digest(&body)),&hexadecimal(&Sha256::digest(&whitespace)));
        assert_eq!(peer.request(&base,"GET",&bad_path,&[]).await.0,StatusCode::NOT_FOUND);
        assert_ne!(peer.request(&base,"GET",&path.replace("data_epoch=","data_epoch=old-"),&[]).await.0,StatusCode::OK);
        assert_ne!(peer.client.get(format!("{base}{path}")).send().await.unwrap().status(),StatusCode::OK);
        let foreign_key = SigningKey::from_slice(&[42u8;32]).unwrap();
        let auth = restarted.auth();
        let foreign_device = auth.pair(&auth.new_pairing_code(),"Other fixture Mac",&STANDARD.encode(foreign_key.verifying_key().to_sec1_bytes())).unwrap();
        let foreign = Peer { key:foreign_key,device:foreign_device.id,last:AtomicI64::new(0),client:reqwest::Client::builder().pool_max_idle_per_host(0).build().unwrap() };
        assert_eq!(foreign.request(&base,"GET",&path,&[]).await.0,StatusCode::NOT_FOUND,
            "another currently approved key cannot read the original actor's action result");
        let mut approvals: Value = serde_json::from_slice(&std::fs::read(home.join("devices.json")).unwrap()).unwrap();
        let entry = approvals.as_array_mut().unwrap().iter_mut().find(|entry|entry["id"]==peer.device).unwrap();
        entry["created_at"] = json!(entry["created_at"].as_i64().unwrap()+1);
        std::fs::write(home.join("devices.json"),serde_json::to_vec(&approvals).unwrap()).unwrap();
        assert_eq!(peer.request(&base,"GET",&path,&[]).await.0,StatusCode::NOT_FOUND,
            "reapproval of the same key cannot inherit the old approval generation's result");
        crate::auth::Auth::open(&home).unwrap().revoke(&peer.device).unwrap();
        assert_ne!(peer.request(&base,"GET",&path,&[]).await.0,StatusCode::OK);
        server.stop().await; drop(restarted);
    }

    #[tokio::test]
    async fn losing_the_initialized_local_journal_cannot_reapply_a_completed_action() {
        let home = Arc::new(crate::test_support::Directory::new());
        std::fs::create_dir_all(&**home).unwrap();
        let peer = Peer::new(&home);
        let (state,base,server) = serve(home.clone()).await;
        let (_,body,path) = action(&state);
        assert_eq!(peer.request(&base,"POST","/actions",&body).await.0,StatusCode::OK);
        let options = alondra_core::options::path_in(&home);
        let changed = std::fs::read_to_string(&options).unwrap().replace("max_working_nodes = 4","max_working_nodes = 5");
        std::fs::write(&options,&changed).unwrap();
        std::fs::remove_file(home.join("action-results.sqlite")).unwrap();
        assert_eq!(peer.request(&base,"GET",&path,&[]).await.0,StatusCode::NOT_FOUND);
        assert_eq!(peer.request(&base,"POST","/actions",&body).await.0,StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(std::fs::read_to_string(&options).unwrap(),changed);
        server.stop().await; drop(state);
    }

    #[tokio::test]
    async fn a_crash_between_the_local_effect_and_receipt_write_remains_unknown_after_restart() {
        let home = Arc::new(crate::test_support::Directory::new());
        std::fs::create_dir_all(&**home).unwrap();
        let peer = Peer::new(&home);
        let (state,base,server) = serve(home.clone()).await;
        let (_,body,path) = action(&state);
        let c = open(&home).unwrap();
        c.execute_batch("CREATE TRIGGER cut_result BEFORE UPDATE OF response ON action_results BEGIN SELECT RAISE(ABORT,'effect-before-result cut'); END;").unwrap();
        assert_eq!(peer.request(&base,"POST","/actions",&body).await.0,StatusCode::SERVICE_UNAVAILABLE);
        let options = alondra_core::options::path_in(&home);
        assert!(std::fs::read_to_string(&options).unwrap().contains("max_working_nodes = 4"));
        c.execute_batch("DROP TRIGGER cut_result;").unwrap(); drop(c);
        let changed = std::fs::read_to_string(&options).unwrap().replace("max_working_nodes = 4","max_working_nodes = 5");
        std::fs::write(&options,&changed).unwrap();
        server.stop().await; drop(state);
        let (restarted,base,server) = serve(home.clone()).await;
        assert_eq!(peer.request(&base,"GET",&path,&[]).await.0,StatusCode::NOT_FOUND);
        assert_eq!(peer.request(&base,"POST","/actions",&body).await.0,StatusCode::CONFLICT);
        assert_eq!(std::fs::read_to_string(&options).unwrap(),changed);
        server.stop().await; drop(restarted);
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::*;
    #[test]
    fn initialized_missing_empty_and_tableless_journals_fail_closed_instead_of_being_recreated() {
        for damage in ["missing","empty","tableless","marker"] {
            let home = crate::test_support::Directory::new();
            std::fs::create_dir_all(&home).unwrap();
            let c = open(&home).unwrap();
            c.execute("INSERT INTO action_results VALUES('actor','id','epoch',?1,'completed',?2)",params![&[1u8;32],b"original result".as_slice()]).unwrap();
            drop(c);
            match damage {
                "missing" => std::fs::remove_file(home.join("action-results.sqlite")).unwrap(),
                "empty" => std::fs::write(home.join("action-results.sqlite"),b"").unwrap(),
                "tableless" => {
                    std::fs::remove_file(home.join("action-results.sqlite")).unwrap();
                    let c = Connection::open(home.join("action-results.sqlite")).unwrap();
                    c.execute_batch("CREATE TABLE unrelated(value TEXT);").unwrap();
                },
                "marker" => std::fs::remove_file(home.join("action-results.schema")).unwrap(),
                _ => unreachable!(),
            }
            assert!(open(&home).is_err(),"{damage} must not initialize a blank action history");
        }
    }
}
