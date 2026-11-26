//! The images a pull request's description cites, read with `gh` or, for an uploaded image's signed
//! link, with a request that carries no credential.
use crate::cli::GhCli;
use alondra_core::markdown::images::IMAGE_MAX_BYTES;
use alondra_engine_ports::github::{Failure, Fetch, GitHubImages, encoded_path, SIGNED_IMAGE_HOST};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(30);

impl GhCli {
    fn web(&self) -> &reqwest::Client {
        self.web.get_or_init(|| {
            reqwest::Client::builder()
                .timeout(LIMIT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default()
        })
    }

    async fn image_api(&self, host: &str, endpoint: &str, accept: &str) -> Result<Vec<u8>, Failure> {
        let host = alondra_domain_model::normalize_pr_host(host);
        let accept = format!("Accept: {accept}");
        let args = ["--hostname", host.as_str(), "-H", accept.as_str(), endpoint];
        match self.api(&args, LIMIT).await {
            Ok(answer) if answer.success() => Ok(answer.body),
            Ok(answer) => Err(match answer.status {
                404 => Failure::NotFound,
                401 => Failure::SignedOut,
                _ => Failure::Transient,
            }),
            Err(failure) if failure.signed_out() => Err(Failure::SignedOut),
            Err(_) => Err(Failure::Transient),
        }
    }
}

impl GitHubImages for GhCli {
    fn size<'a>(&'a self, host: &'a str, repository: &'a str, path: &'a str, reference: &'a str) -> Fetch<'a, u64> {
        Box::pin(async move {
            let endpoint = format!("repos/{repository}/contents/{path}?ref={}", encoded_path(reference));
            let out = self.image_api(host, &endpoint, "application/vnd.github.object+json").await?;
            let value: serde_json::Value = serde_json::from_slice(&out).map_err(|_| Failure::Transient)?;
            match value["type"].as_str() {
                Some("file") => value["size"].as_u64().ok_or(Failure::Transient),
                _ => Err(Failure::NotFound),
            }
        })
    }

    fn file<'a>(&'a self, host: &'a str, repository: &'a str, path: &'a str, reference: &'a str) -> Fetch<'a, Vec<u8>> {
        Box::pin(async move {
            let endpoint = format!("repos/{repository}/contents/{path}?ref={}", encoded_path(reference));
            self.image_api(host, &endpoint, "application/vnd.github.raw").await
        })
    }

    fn repository<'a>(&'a self, host: &'a str, repository: &'a str) -> Fetch<'a, ()> {
        Box::pin(async move { self.image_api(host, &format!("repos/{repository}"), "application/vnd.github+json").await.map(|_| ()) })
    }

    fn body_html<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Fetch<'a, String> {
        Box::pin(async move {
            let out = self.image_api(host, &format!("repos/{repository}/pulls/{number}"), "application/vnd.github.full+json").await?;
            let value: serde_json::Value = serde_json::from_slice(&out).map_err(|_| Failure::Transient)?;
            Ok(value["body_html"].as_str().unwrap_or_default().to_owned())
        })
    }

    fn download<'a>(&'a self, url: &'a str) -> Fetch<'a, Vec<u8>> {
        Box::pin(async move {
            if !url.strip_prefix("https://").is_some_and(|rest| rest.starts_with(&format!("{SIGNED_IMAGE_HOST}/"))) {
                return Err(Failure::NotFound);
            }
            let mut response = self.web().get(url).send().await.map_err(|_| Failure::Transient)?;
            match response.status().as_u16() {
                200 => {}
                403 | 404 | 410 => return Err(Failure::NotFound),
                _ => return Err(Failure::Transient),
            }
            if let Some(bytes) = response.content_length().filter(|bytes| *bytes > IMAGE_MAX_BYTES) {
                return Err(Failure::TooLarge(bytes));
            }
            let mut bytes = vec![];
            while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Transient)? {
                bytes.extend_from_slice(&chunk);
                if bytes.len() as u64 > IMAGE_MAX_BYTES {
                    return Err(Failure::TooLarge(bytes.len() as u64));
                }
            }
            Ok(bytes)
        })
    }
}
