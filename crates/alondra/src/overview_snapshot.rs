//! Bounded, opt-in transport for an overview that does not fit one encrypted envelope.
//! This is a read-only snapshot, not a retry mechanism for mutations.
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const ROOT: &str = "/v1/service/overview-snapshot";
const CHUNK_BYTES: usize = 65_536;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_DURATION: Duration = Duration::from_secs(90);

#[derive(Deserialize)]
struct Chunk {
    schema: u32,
    snapshot_id: String,
    total_bytes: usize,
    sha256: String,
    chunk_bytes: usize,
    offset: usize,
    data: String,
}

fn lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn remaining(deadline: Instant, per_request: Duration) -> Result<Duration> {
    let left = deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .context("overview snapshot deadline exceeded; the previous overview is unchanged")?;
    Ok(left.min(per_request))
}

pub(crate) fn read(
    timeout: Duration,
    call: impl FnMut(&str, &str, Duration) -> Result<(u16, Value)>,
) -> Result<(u16, Value)> {
    read_until(timeout, Instant::now() + MAX_DURATION, call)
}

fn read_until(
    timeout: Duration,
    deadline: Instant,
    mut call: impl FnMut(&str, &str, Duration) -> Result<(u16, Value)>,
) -> Result<(u16, Value)> {
    let legacy = call("GET", "/v1/overview", remaining(deadline, timeout)?)?;
    // Authentication, transport and other endpoint errors retain their original semantics.
    // Only the explicit envelope-size failure negotiates the bounded read extension.
    if legacy.0 != 502 || legacy.1["error"] != "response_too_large" {
        return Ok(legacy);
    }
    let (status, value) = call("GET", ROOT, remaining(deadline, timeout)?)?;
    ensure!(
        status == 200,
        "overview snapshot unavailable (HTTP {status}); the destination may need an update"
    );
    let first: Chunk =
        serde_json::from_value(value).context("invalid overview snapshot metadata")?;
    ensure!(
        lower_hex(&first.snapshot_id, 32),
        "invalid overview snapshot identifier"
    );
    let path = format!("{ROOT}/{}", first.snapshot_id);
    let result = assemble(first, deadline, timeout, &mut call);
    // Release is best effort and cannot turn a successful read into an error, mask an
    // integrity error or exceed the overall deadline. Server TTL is the final cleanup.
    if let Ok(left) = remaining(deadline, Duration::from_secs(2)) {
        let _ = call("DELETE", &path, left);
    }
    result.map(|value| (200, value))
}

fn assemble(
    first: Chunk,
    deadline: Instant,
    timeout: Duration,
    call: &mut impl FnMut(&str, &str, Duration) -> Result<(u16, Value)>,
) -> Result<Value> {
    ensure!(
        first.schema == 1 && first.chunk_bytes == CHUNK_BYTES,
        "unsupported overview snapshot format"
    );
    ensure!(
        (1..=MAX_BYTES).contains(&first.total_bytes),
        "overview snapshot exceeds the 8 MiB client limit"
    );
    ensure!(
        lower_hex(&first.sha256, 64),
        "invalid overview snapshot digest"
    );
    let id = first.snapshot_id.clone();
    let total = first.total_bytes;
    let expected_hash = first.sha256.clone();
    let mut chunk = first;
    // Do not reserve from remote metadata. Storage grows only for verified, bounded chunks.
    let mut bytes = Vec::new();
    let mut hash = Sha256::new();
    loop {
        remaining(deadline, timeout)?;
        ensure!(
            chunk.schema == 1
                && chunk.snapshot_id == id
                && chunk.total_bytes == total
                && chunk.sha256 == expected_hash
                && chunk.chunk_bytes == CHUNK_BYTES,
            "overview snapshot changed while downloading"
        );
        ensure!(
            chunk.offset == bytes.len(),
            "overview snapshot offset mismatch"
        );
        ensure!(
            chunk.data.len() <= CHUNK_BYTES.div_ceil(3) * 4,
            "overview snapshot chunk exceeds its limit"
        );
        let data = B64
            .decode(&chunk.data)
            .context("invalid overview snapshot chunk encoding")?;
        ensure!(
            data.len() == CHUNK_BYTES.min(total - bytes.len()),
            "overview snapshot chunk is missing or has the wrong length"
        );
        hash.update(&data);
        bytes.extend_from_slice(&data);
        if bytes.len() == total {
            break;
        }
        let path = format!("{ROOT}/{id}?offset={}", bytes.len());
        let (status, value) = call("GET", &path, remaining(deadline, timeout)?)?;
        ensure!(
            status == 200,
            "overview snapshot read failed (HTTP {status}); no partial overview was applied"
        );
        chunk = serde_json::from_value(value).context("invalid overview snapshot chunk")?;
    }
    let actual = format!("{:x}", hash.finalize());
    ensure!(
        actual == expected_hash,
        "overview snapshot digest mismatch; no partial overview was applied"
    );
    serde_json::from_slice(&bytes).context("overview snapshot is not valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const ID: &str = "0123456789abcdef0123456789abcdef";
    fn document(len: usize) -> Vec<u8> {
        serde_json::to_vec(&json!({"tick": 42, "document": "á".repeat(len)})).unwrap()
    }
    fn chunk(bytes: &[u8], offset: usize) -> Value {
        json!({"schema":1,"snapshot_id":ID,"total_bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes)),
            "chunk_bytes":CHUNK_BYTES,"offset":offset,"data":B64.encode(&bytes[offset..(offset+CHUNK_BYTES).min(bytes.len())])})
    }
    fn run(
        bytes: &[u8],
        mut alter: impl FnMut(&str, &mut Value),
    ) -> (Result<(u16, Value)>, Vec<(String, String)>) {
        let mut calls = vec![];
        let result = read(Duration::from_secs(15), |method, path, _| {
            calls.push((method.into(), path.into()));
            if path == "/v1/overview" {
                return Ok((502, json!({"error":"response_too_large"})));
            }
            if method == "DELETE" {
                return Ok((200, json!({"released":true})));
            }
            let offset = path
                .split_once("?offset=")
                .map(|(_, n)| n.parse().unwrap())
                .unwrap_or(0);
            let mut value = chunk(bytes, offset);
            alter(path, &mut value);
            Ok((200, value))
        });
        (result, calls)
    }
    #[test]
    fn large_unicode_overview_reassembles_exactly_and_releases_its_snapshot() {
        let bytes = document(150_000);
        assert!(bytes.len() > 180 * 1024);
        let (result, calls) = run(&bytes, |_, _| {});
        assert_eq!(
            result.unwrap(),
            (200, serde_json::from_slice(&bytes).unwrap())
        );
        assert_eq!(
            calls.last().unwrap(),
            &("DELETE".into(), format!("{ROOT}/{ID}"))
        );
        assert_eq!(calls.len(), bytes.len().div_ceil(CHUNK_BYTES) + 2);
    }
    #[test]
    fn exact_total_limit_is_accepted_without_unbounded_preallocation() {
        let mut bytes = vec![b' '; MAX_BYTES];
        bytes[0] = b'[';
        bytes[1] = b'0';
        bytes[MAX_BYTES - 1] = b']';
        let (result, calls) = run(&bytes, |_, _| {});
        assert_eq!(result.unwrap(), (200, json!([0])));
        assert_eq!(calls.len(), MAX_BYTES / CHUNK_BYTES + 2);
    }
    #[test]
    fn valid_json_content_corruption_is_caught_by_whole_document_hash() {
        let bytes = serde_json::to_vec(&json!({"v":"a".repeat(200_000)})).unwrap();
        let (result, _) = run(&bytes, |path, value| {
            if path == ROOT {
                let mut data = B64.decode(value["data"].as_str().unwrap()).unwrap();
                data[100] = b'b';
                value["data"] = json!(B64.encode(data));
            }
        });
        assert!(result.unwrap_err().to_string().contains("digest mismatch"));
    }

    #[test]
    fn legacy_success_and_unrelated_errors_never_trigger_another_request() {
        for status in [200, 401, 403, 404, 429, 502, 503] {
            let original = json!({"error":"unrelated","tick":12});
            let mut count = 0;
            let result = read(Duration::from_secs(1), |method, path, _| {
                count += 1;
                assert_eq!((method, path), ("GET", "/v1/overview"));
                Ok((status, original.clone()))
            })
            .unwrap();
            assert_eq!(result, (status, original));
            assert_eq!(count, 1);
        }
    }
    #[test]
    fn tampering_and_changed_snapshot_metadata_fail_before_applying_and_still_release() {
        let bytes = document(100_000);
        for field in [
            "snapshot_id",
            "total_bytes",
            "sha256",
            "chunk_bytes",
            "offset",
            "data",
            "schema",
        ] {
            let (result, calls) = run(&bytes, |path, value| {
                if path.contains("?offset=") {
                    value[field] = match field {
                        "snapshot_id" => json!("11111111111111111111111111111111"),
                        "sha256" => json!("0".repeat(64)),
                        "data" => json!(B64.encode(vec![b'x'; CHUNK_BYTES])),
                        _ => json!(0),
                    };
                }
            });
            assert!(result.is_err(), "accepted corrupted {field}");
            assert_eq!(calls.last().unwrap().0, "DELETE");
        }
    }
    #[test]
    fn announced_oversize_empty_and_bad_format_are_rejected_without_fetching_chunks() {
        let bytes = document(100);
        for (field, bad) in [
            ("total_bytes", json!(MAX_BYTES + 1)),
            ("total_bytes", json!(0)),
            ("chunk_bytes", json!(1)),
            ("schema", json!(2)),
            ("sha256", json!("bad")),
            ("offset", json!(1)),
            ("data", json!("!")),
        ] {
            let (result, calls) = run(&bytes, |_, v| v[field] = bad.clone());
            assert!(result.is_err(), "{field}");
            assert_eq!(calls.iter().filter(|(m, _)| m == "GET").count(), 2);
        }
    }
    #[test]
    fn truncation_and_oversized_encoded_chunks_are_rejected() {
        let bytes = document(100_000);
        for bad in [
            String::new(),
            "a".repeat(CHUNK_BYTES.div_ceil(3) * 4 + 1),
            B64.encode(vec![0; CHUNK_BYTES - 1]),
        ] {
            let (result, _) = run(&bytes, |_, v| v["data"] = json!(bad));
            assert!(result.is_err());
        }
    }
    #[test]
    fn revoked_missing_expired_or_saturated_snapshot_never_restarts_or_falls_back() {
        let bytes = document(100_000);
        for status in [401, 403, 404, 410, 429, 503] {
            let mut calls = vec![];
            let result = read(Duration::from_secs(1), |method, path, _| {
                calls.push((method.to_owned(), path.to_owned()));
                if path == "/v1/overview" {
                    Ok((502, json!({"error":"response_too_large"})))
                } else if path == ROOT {
                    Ok((200, chunk(&bytes, 0)))
                } else {
                    Ok((status, json!({"error":"fixture"})))
                }
            });
            assert!(result.is_err());
            assert_eq!(calls.len(), 4);
            assert_eq!(calls.last().unwrap().0, "DELETE");
        }
    }
    #[test]
    fn deadline_prevents_any_request_and_cleanup_does_not_mask_integrity_failure() {
        assert!(
            read_until(Duration::from_secs(1), Instant::now(), |_, _, _| panic!(
                "expired request"
            ))
            .is_err()
        );
        let bytes = document(10);
        let mut calls = 0;
        let result = read(Duration::from_secs(1), |method, path, _| {
            calls += 1;
            if path == "/v1/overview" {
                return Ok((502, json!({"error":"response_too_large"})));
            }
            if method == "DELETE" {
                anyhow::bail!("cleanup unavailable");
            }
            let mut v = chunk(&bytes, 0);
            v["sha256"] = json!("0".repeat(64));
            Ok((200, v))
        });
        assert!(result.unwrap_err().to_string().contains("digest mismatch"));
        assert_eq!(calls, 3);
    }
}
