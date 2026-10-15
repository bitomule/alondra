//! The synthetic phone over a Mac's iroh endpoint, as the apps will use it: one connection opened
//! with a hello signed by the paired device key, then one encrypted envelope per stream.
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use iroh::endpoint::{Connection, PortmapperConfig, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use p256::ecdsa::SigningKey;
use serde_json::Value;

use super::{Link, Phone};
use crate::sandbox::{Target, random};

const ALPN: &[u8] = b"alondra/1";
const MAX_REPLY: usize = 8 * 1024 * 1024;

/// One open connection to a Mac, with the runtime that keeps it alive.
pub struct IrohSession {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    connection: Connection,
}

impl Drop for IrohSession {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"done");
        self.runtime.block_on(self.endpoint.close());
    }
}

impl IrohSession {
    /// Sends one sealed envelope on its own stream and returns the Mac's reply: `{"response": …}`,
    /// `{"revoked": …}` or `{"error": …}`.
    pub fn send(&self, sealed: &[u8]) -> Result<Value> {
        self.runtime.block_on(exchange(&self.connection, sealed))
    }
}

async fn exchange(connection: &Connection, body: &[u8]) -> Result<Value> {
    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(body).await?;
    send.finish()?;
    let reply = tokio::time::timeout(Duration::from_secs(30), recv.read_to_end(MAX_REPLY))
        .await
        .context("the Mac did not answer on iroh within 30 s")??;
    Ok(serde_json::from_slice(&reply)?)
}

impl Phone<'_> {
    /// The iroh endpoint ID the Mac's direct listener gives in its hello.
    pub fn mac_iroh_endpoint(&self, target: &Target) -> Result<EndpointId> {
        let hello = super::direct::hello(target)?;
        let id = hello["iroh_endpoint"].as_str().ok_or_else(|| anyhow!("Mac {} names no iroh endpoint", target.number))?;
        Ok(id.parse()?)
    }

    /// Connects to the Mac's iroh endpoint and says hello as `link`'s device. A refused hello is
    /// an error naming what the Mac answered.
    pub fn connect_iroh(&self, target: &Target, link: &Link) -> Result<IrohSession> {
        let mac = self.mac_iroh_endpoint(target)?;
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
        let (endpoint, connection) = runtime.block_on(async {
            let endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(SecretKey::from_bytes(&random::<32>()?))
                .relay_mode(RelayMode::Disabled)
                .portmapper_config(PortmapperConfig::Disabled)
                .clear_ip_transports()
                .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))?
                .bind()
                .await?;
            let connection = endpoint.connect(EndpointAddr::new(mac).with_ip_addr(target.iroh), ALPN).await?;
            let signature = alondra_e2e::sign_iroh_hello(
                &link.signing,
                &target.mac.id,
                &link.device_id,
                endpoint.id().as_bytes(),
                mac.as_bytes(),
            )
            .map_err(|error| anyhow!("sign the hello: {error:?}"))?;
            let hello = serde_json::json!({"device_id": link.device_id, "signature": signature});
            let reply = exchange(&connection, &serde_json::to_vec(&hello)?).await?;
            ensure!(reply == serde_json::json!({"ok": true}), "the Mac refused the iroh hello: {reply}");
            anyhow::Ok((endpoint, connection))
        })?;
        Ok(IrohSession { runtime, endpoint, connection })
    }

    /// A device this Mac never paired, with keys of its own.
    pub fn stranger(&self) -> Result<Link> {
        Ok(Link {
            device_id: format!("stranger-{}", crate::sandbox::hex(&random::<6>()?)),
            signing: SigningKey::from_slice(&random::<32>()?)?,
            agreement: p256::SecretKey::from_slice(&random::<32>()?)?,
        })
    }
}
