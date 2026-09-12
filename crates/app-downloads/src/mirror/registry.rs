use super::{digest_hex, Layer, Result, MAX_METADATA, REPOSITORY};
use reqwest::{header, Client, Response, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Write, time::Duration};
use zeroize::Zeroizing;

/// No Debug implementation: both registry and account tokens stay private.
pub struct Registry {
    client: Client,
    authorization: header::HeaderValue,
}

impl Registry {
    pub async fn authenticate(username: &str, secret: &str) -> Result<Self> {
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|_| "could not initialize HTTPS client")?;
        let response = client
            .get("https://ghcr.io/token")
            .query(&[
                ("service", "ghcr.io"),
                ("scope", &format!("repository:{REPOSITORY}:pull")),
            ])
            .basic_auth(username, Some(secret))
            .send()
            .await
            .map_err(|_| "GHCR authentication transport failed")?;
        let bytes = Zeroizing::new(bounded(response, 64 * 1024).await?);
        #[derive(Deserialize)]
        struct Token {
            token: String,
        }
        let reply: Token = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid registry authentication response")?;
        let token = Zeroizing::new(reply.token);
        if token.is_empty() || token.len() > 32 * 1024 {
            return Err("invalid registry token");
        }
        let value = Zeroizing::new(format!("Bearer {}", token.as_str()));
        let mut authorization =
            header::HeaderValue::from_str(&value).map_err(|_| "invalid registry token")?;
        authorization.set_sensitive(true);
        Ok(Self {
            client,
            authorization,
        })
    }

    pub async fn manifest(&self, digest: &str) -> Result<Vec<u8>> {
        digest_hex(digest)?;
        self.manifest_reference(digest).await
    }

    pub async fn stable_manifest(&self) -> Result<Vec<u8>> {
        self.manifest_reference("stable").await
    }

    async fn manifest_reference(&self, digest: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(format!(
                "https://ghcr.io/v2/{REPOSITORY}/manifests/{digest}"
            ))
            .header(header::AUTHORIZATION, self.authorization.clone())
            .header(header::ACCEPT, "application/vnd.oci.image.manifest.v1+json")
            .send()
            .await
            .map_err(|_| "GHCR manifest request failed")?;
        bounded(response, MAX_METADATA).await
    }

    async fn blob(&self, digest: &str) -> Result<Response> {
        digest_hex(digest)?;
        let mut response = self
            .client
            .get(format!("https://ghcr.io/v2/{REPOSITORY}/blobs/{digest}"))
            .header(header::AUTHORIZATION, self.authorization.clone())
            .send()
            .await
            .map_err(|_| "GHCR blob request failed")?;
        for _ in 0..3 {
            if !response.status().is_redirection() {
                return Ok(response);
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or("invalid GHCR redirect")?;
            let url = checked_redirect(location)?;
            // Deliberately no account/registry Authorization on the CDN request.
            response = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|_| "artifact CDN request failed")?;
        }
        Err("too many artifact redirects")
    }

    pub async fn descriptor(&self, layer: &Layer) -> Result<Vec<u8>> {
        bounded(self.blob(&layer.digest).await?, MAX_METADATA).await
    }

    pub async fn download(&self, layer: &Layer, output: &mut File) -> Result<()> {
        stream_to_file(self.blob(&layer.digest).await?, layer, output).await
    }
}

async fn stream_to_file(mut response: Response, layer: &Layer, output: &mut File) -> Result<()> {
    if !response.status().is_success() || response.content_length().is_some_and(|n| n != layer.size)
    {
        return Err("artifact download refused or size mismatch");
    }
    let mut size = 0_u64;
    let mut hash = Sha256::new();
    while let Some(bytes) = response
        .chunk()
        .await
        .map_err(|_| "artifact download interrupted")?
    {
        size = size
            .checked_add(bytes.len() as u64)
            .ok_or("artifact exceeds size limit")?;
        if size > layer.size {
            return Err("artifact exceeds declared size");
        }
        hash.update(&bytes);
        output
            .write_all(&bytes)
            .map_err(|_| "artifact staging write failed")?;
    }
    if size != layer.size || hex::encode(hash.finalize()) != digest_hex(&layer.digest)? {
        return Err("downloaded artifact checksum or size mismatch");
    }
    output
        .sync_all()
        .map_err(|_| "artifact staging sync failed")
}

async fn bounded(mut response: Response, maximum: usize) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        return Err("GHCR denied request or is unavailable");
    }
    if response
        .content_length()
        .is_some_and(|n| n > maximum as u64)
    {
        return Err("remote metadata exceeds limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "metadata download interrupted")?
    {
        if chunk.len() > maximum - bytes.len() {
            return Err("remote metadata exceeds limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn checked_redirect(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| "invalid artifact redirect")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.fragment().is_some()
        || url.host_str() != Some("pkg-containers.githubusercontent.com")
    {
        return Err("untrusted artifact redirect");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(status: u16, bytes: &'static str) -> Response {
        http::Response::builder()
            .status(status)
            .body(bytes)
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn metadata_bounds_and_denials_do_not_echo_response_bodies() {
        assert_eq!(bounded(response(200, "1234"), 4).await.unwrap(), b"1234");
        assert!(bounded(response(200, "12345"), 4).await.is_err());
        for status in [401, 403, 429, 500, 302] {
            let error = bounded(response(status, "fixture-private-body"), 100)
                .await
                .unwrap_err();
            assert!(!error.contains("fixture-private-body"));
        }
    }

    #[tokio::test]
    async fn stream_checks_length_and_hash_before_accepting_download() {
        let layer = Layer {
            digest: format!("sha256:{}", hex::encode(Sha256::digest(b"1234"))),
            size: 4,
            media_type: "application/octet-stream".into(),
            annotations: Default::default(),
        };
        for body in ["123", "12345", "4321"] {
            let mut file = tempfile::tempfile().unwrap();
            assert!(stream_to_file(response(200, body), &layer, &mut file)
                .await
                .is_err());
        }
        let mut file = tempfile::tempfile().unwrap();
        stream_to_file(response(200, "1234"), &layer, &mut file)
            .await
            .unwrap();
        assert_eq!(file.metadata().unwrap().len(), 4);
        assert!(stream_to_file(response(403, "1234"), &layer, &mut file)
            .await
            .is_err());
    }
    #[test]
    fn redirects_never_accept_arbitrary_hosts_credentials_or_cleartext() {
        assert!(
            checked_redirect("https://pkg-containers.githubusercontent.com/fixture?sig=test")
                .is_ok()
        );
        for url in [
            "http://pkg-containers.githubusercontent.com/a",
            "https://example.com/a",
            "https://pkg-containers.githubusercontent.com.evil.example/a",
            "https://127.0.0.1/a",
            "https://user:secret@pkg-containers.githubusercontent.com/a",
            "https://pkg-containers.githubusercontent.com:444/a",
        ] {
            assert!(checked_redirect(url).is_err());
        }
    }
}
