//! A bounded pair of signed TUI pollers, driven explicitly without waiting for elapsed time.
use std::{cell::Cell, future::{Future, poll_fn}, task::Poll};

use alondra_view_model::{Transport, VIEWS_PROTOCOL};
use axum::{http::{HeaderMap, StatusCode}, response::Response};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::Value;

async fn body(response: Response) -> Result<Value, String> {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.map_err(|error| error.to_string())?;
    if status != StatusCode::OK { return Err(format!("feed returned {status}")); }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

pub(crate) async fn shared_tui_feed_work(
    client: &crate::client_routes::Client, auth: &crate::auth::Auth, key: &SigningKey, device: &str,
    first_timestamp: i64,
) -> Result<(u64, usize), String> {
    let initial = body(super::feed::handle(client, device,
        &format!("protocol={VIEWS_PROTOCOL}&client=tui&lang=en&wait=0"), Transport::Loopback).await).await?;
    let cursor = initial["cursor"].as_str().ok_or("initial feed has no cursor")?;
    let query = format!("protocol={VIEWS_PROTOCOL}&client=tui&lang=en&wait=50&cursor={cursor}");
    let path = format!("/changes?{query}");
    let before = auth.request_crypto_counts_for_test().2;
    let progress_before = client.feeds.test_progress();
    let requests = Cell::new(0u64);
    let timestamp = Cell::new(first_timestamp);
    let poller = || async {
        loop {
            if requests.get() >= 64 { return Err("shared TUI probe exceeded 64 requests".to_owned()); }
            timestamp.set(timestamp.get() + 1);
            let stamp = timestamp.get().to_string();
            let signature: Signature = key.sign(crate::auth::signed_message("GET", &path, &stamp, b"").as_bytes());
            let mut headers = HeaderMap::new();
            headers.insert("x-alondra-device", device.parse().map_err(|_| "invalid fixture device")?);
            headers.insert("x-alondra-timestamp", stamp.parse().map_err(|_| "invalid fixture timestamp")?);
            headers.insert("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()).parse()
                .map_err(|_| "invalid fixture signature")?);
            let signed = auth.verify_request_for_test(&headers, "GET", &path, b"", true)
                .map_err(str::to_owned)?;
            requests.set(requests.get() + 1);
            let reply = body(super::feed::handle(client, &signed.device_id, &query, Transport::Loopback).await).await?;
            if reply["superseded"] != true {
                return Err("shared TUI probe received an unexpected non-superseded response".to_owned());
            }
            // The next explicit poll starts the next request; no scheduler timing is asserted.
            tokio::task::yield_now().await;
        }
    };
    let mut first = Box::pin(poller());
    let mut second = Box::pin(poller());
    poll_fn(|cx| {
        for _ in 0..8 {
            for future in [&mut first, &mut second] {
                if let Poll::Ready(result) = future.as_mut().poll(cx) { return Poll::Ready(result); }
            }
            let (waiting, displacements) = client.feeds.test_progress();
            if waiting - progress_before.0 >= 2 && displacements - progress_before.1 >= 1 {
                return Poll::Ready(Ok(()));
            }
        }
        Poll::Pending
    }).await?;
    for _ in 0..8 {
        poll_fn(|cx| {
            for future in [&mut first, &mut second] {
                if let Poll::Ready(result) = future.as_mut().poll(cx) { return Poll::Ready(result); }
            }
            Poll::Ready(Ok(()))
        }).await?;
    }
    drop((first, second));
    let calls = requests.get();
    if calls < 2 { return Err(format!("shared TUI probe admitted only {calls} requests")); }
    let verifications = auth.request_crypto_counts_for_test().2 - before;
    if verifications != calls as usize {
        return Err(format!("shared TUI probe measured {calls} requests but {verifications} verifications"));
    }
    Ok((calls, verifications))
}
