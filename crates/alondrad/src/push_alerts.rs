//! How the usage pause tells the user: a push to each device in its language, through the
//! service when one is configured.

/// Pushes to the user's devices, in each device's language.
pub(crate) struct PushAlerts {
    pub domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>,
    pub push: std::sync::Arc<crate::push::Push>,
    pub service: Option<std::sync::Arc<crate::service::Service>>,
    pub client: std::sync::Arc<crate::client_routes::Client>,
}

impl crate::usage_pause::Alerts for PushAlerts {
    async fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, passive: bool) {
        let epoch = self.domain.state().map(|state| state.data_epoch.clone()).ok();
        let payload = |spanish: bool| serde_json::json!({
            "aps": {
                "alert": {"title": "Alondra", "body": text(spanish)},
                "sound": "default",
                "interruption-level": if passive { "passive" } else { "active" },
                "mutable-content": 1,
            },
            "alondra": {"protocol": alondra_core::views::VIEWS_PROTOCOL, "data_epoch": epoch, "actions": []},
        });
        if let Some(service) = &self.service {
            let sent = service.broadcast_push_report_for_device(!passive, |device| {
                payload(self.client.language_for_device(device) == alondra_core::views::Lang::Es)
            }).await;
            if let Err(error) = sent {
                log!("could not push the notice: {error:#}");
            }
        } else if self.push.enabled() {
            self.push.broadcast(&payload(false), !passive).await;
        }
    }
}
