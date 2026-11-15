//! Where a push leaves the daemon: the relay in production, a recorder in a scenario.

use crate::service::{PushDelivery, Service};
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// One push for one phone, after the quiet hours were applied to it.
pub struct DevicePush<'a> {
    pub device: &'a str,
    pub binding: &'a str,
    pub delivery: PushDelivery,
    pub with_sound: bool,
    pub payload: &'a Value,
}

pub type Sent<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

pub trait PushTransport: Send + Sync + 'static {
    /// Whether any phone can be reached at all; without it a broadcast sends nothing.
    fn reaches_devices(&self, service: &Service) -> bool;
    fn send<'a>(&'a self, service: &'a Service, push: DevicePush<'a>, epoch: &'a mut Option<[u8; 16]>) -> Sent<'a>;
}

/// Encrypts the push for the phone and hands it to the Alondra relay.
pub struct RelayPush;

impl PushTransport for RelayPush {
    fn reaches_devices(&self, service: &Service) -> bool {
        service.has_relay()
    }

    fn send<'a>(&'a self, service: &'a Service, push: DevicePush<'a>, epoch: &'a mut Option<[u8; 16]>) -> Sent<'a> {
        Box::pin(service.send_through_relay(push, epoch))
    }
}

/// The transport a service carries; the relay unless a scenario swaps it.
#[derive(Clone)]
pub struct Pushes(Arc<dyn PushTransport>);

impl Pushes {
    pub fn new(transport: impl PushTransport) -> Self {
        Self(Arc::new(transport))
    }
    pub(crate) fn get(&self) -> &dyn PushTransport {
        &*self.0
    }
}

impl Default for Pushes {
    fn default() -> Self {
        Self::new(RelayPush)
    }
}
