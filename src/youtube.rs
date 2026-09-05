use crate::{model::*, store::Store};
use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};

pub const SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";
const API: &str = "https://www.googleapis.com/youtube/v3/videos";
const UPLOAD: &str = "https://www.googleapis.com/upload/youtube/v3/videos";
const CHUNK: usize = 8 * 1024 * 1024;
const TOKEN: &str = "https://oauth2.googleapis.com/token";
/// Google's endpoints in production; tests point these at a local fake peer.
pub struct Endpoints {
    pub api: String,
    pub upload: String,
    pub token: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api: API.into(),
            upload: UPLOAD.into(),
            token: TOKEN.into(),
        }
    }
}
#[derive(Debug)]
pub struct RemoteError {
    pub message: String,
    pub retryable: bool,
    pub delay: f64,
}
impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for RemoteError {}
pub fn remote_error(message: &str, retryable: bool, delay: f64) -> anyhow::Error {
    RemoteError {
        message: message.into(),
        retryable,
        delay,
    }
    .into()
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(alias = "access_token")]
    pub token: String,
    pub refresh_token: String,
    pub client_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub expires_at: f64,
    #[serde(default)]
    pub scopes: Vec<String>,
}
#[derive(Clone, Deserialize)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_secret: String,
}
pub fn load_client(path: &Path) -> Result<OAuthClient> {
    let value: Value = serde_json::from_slice(
        &std::fs::read(path).context("OAuth client JSON is not configured")?,
    )?;
    serde_json::from_value(
        value
            .get("web")
            .context("Create a Web application OAuth client for this web UI")?
            .clone(),
    )
    .map_err(Into::into)
}
pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
pub fn save_credentials(path: &Path, c: &Credentials) -> Result<()> {
    use std::io::Write;
    let mut temp =
        tempfile::NamedTempFile::new_in(path.parent().context("Token needs a parent directory")?)?;
    temp.write_all(&serde_json::to_vec(c)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}
pub struct YouTube {
    pub http: Client,
    pub credentials: Mutex<Option<Credentials>>,
    pub store: Arc<Store>,
    pub endpoints: Endpoints,
}
impl YouTube {
    pub fn new(store: Arc<Store>) -> Result<Self> {
        Self::with_endpoints(store, Endpoints::default())
    }
    pub fn with_endpoints(store: Arc<Store>, endpoints: Endpoints) -> Result<Self> {
        let path = store.dir.join("token.json");
        let creds = if path.exists() {
            Some(
                serde_json::from_slice(&std::fs::read(path)?)
                    .context("Invalid saved OAuth credentials")?,
            )
        } else {
            None
        };
        Ok(Self {
            http: client()?,
            credentials: Mutex::new(creds),
            store,
            endpoints,
        })
    }
    pub async fn connected(&self) -> bool {
        self.credentials.lock().await.is_some()
    }
    pub async fn access_token(&self) -> Result<String> {
        let mut guard = self.credentials.lock().await;
        let c = guard.as_mut().context("Connect YouTube before uploading")?;
        if !c.scopes.iter().any(|s| s == SCOPE) {
            bail!("Reconnect YouTube to grant the scheduling scope");
        }
        if c.expires_at < now() + 120.0 {
            let response = self
                .http
                .post(&self.endpoints.token)
                .form(&[
                    ("client_id", c.client_id.as_str()),
                    ("client_secret", c.client_secret.as_str()),
                    ("refresh_token", c.refresh_token.as_str()),
                    ("grant_type", "refresh_token"),
                ])
                .send()
                .await
                .map_err(|_| {
                    remote_error("Cannot reach Google to refresh authorization", true, 0.0)
                })?;
            if !response.status().is_success() {
                return Err(remote_error(
                    "Google authorization expired or was revoked. Reconnect the same channel.",
                    false,
                    0.0,
                ));
            }
            let v: Value = response.json().await?;
            c.token = v["access_token"]
                .as_str()
                .context("Google returned no access token")?
                .into();
            c.expires_at = now() + v["expires_in"].as_f64().unwrap_or(3600.0);
            if let Some(r) = v["refresh_token"].as_str() {
                c.refresh_token = r.into();
            }
            save_credentials(&self.store.dir.join("token.json"), c)?;
        }
        Ok(c.token.clone())
    }
    /// Production keeps the strict Google check; a test peer is confined to its own base URL.
    fn check_session(&self, url: &str) -> Result<()> {
        if self.endpoints.upload == UPLOAD {
            validate_session(url)
        } else if url.starts_with(self.endpoints.upload.as_str()) {
            Ok(())
        } else {
            bail!("Unexpected upload session URL")
        }
    }
    pub async fn request(
        &self,
        method: Method,
        url: &str,
        headers: Vec<(&str, String)>,
        body: Option<Value>,
        bytes: Option<Vec<u8>>,
    ) -> Result<Response> {
        let token = self.access_token().await?;
        let mut req = self.http.request(method, url).bearer_auth(token);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(v) = body {
            req = req.json(&v)
        }
        if let Some(v) = bytes {
            req = req.body(v)
        }
        let response = req.send().await.map_err(|_| {
            remote_error(
                "Network request failed; the saved upload will be reconciled",
                true,
                0.0,
            )
        })?;
        let status = response.status().as_u16();
        if status == 200 || status == 201 || status == 308 {
            return Ok(response);
        }
        let delay = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(retry_after)
            .unwrap_or(0.0);
        let v: Value = response.json().await.unwrap_or(Value::Null);
        let reason = v["error"]["errors"][0]["reason"]
            .as_str()
            .filter(|s| s.len() < 80 && s.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("unknown");
        if matches!(status, 404 | 410) && url.starts_with(self.endpoints.upload.as_str()) {
            return Err(remote_error(
                "Upload session expired. Check YouTube Studio before creating another job; no duplicate upload will be started.",
                false,
                0.0,
            ));
        }
        let retryable = matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
            || matches!(
                reason,
                "quotaExceeded" | "rateLimitExceeded" | "userRateLimitExceeded" | "backendError"
            );
        Err(remote_error(
            &format!("YouTube HTTP {status}: {reason}"),
            retryable,
            delay,
        ))
    }
    pub async fn start(&self, j: &Job) -> Result<String> {
        let r = self
            .request(
                Method::POST,
                &format!(
                    "{}?uploadType=resumable&part=snippet,status",
                    self.endpoints.upload
                ),
                vec![
                    ("X-Upload-Content-Length", j.size.to_string()),
                    ("X-Upload-Content-Type", "application/octet-stream".into()),
                ],
                Some(upload_body(j)),
                None,
            )
            .await?;
        let url = r
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .context("YouTube returned no upload session")?;
        self.check_session(url)?;
        Ok(url.into())
    }
    pub async fn upload(&self, j: &Job) -> Result<String> {
        let url = j
            .session_url
            .as_deref()
            .context("Missing saved upload session")?;
        self.check_session(url)?;
        let mut r = self
            .request(
                Method::PUT,
                url,
                vec![
                    ("Content-Length", "0".into()),
                    ("Content-Range", format!("bytes */{}", j.size)),
                ],
                None,
                Some(vec![]),
            )
            .await?;
        if r.status().as_u16() != 308 {
            return completion(r).await;
        }
        let mut f = tokio::fs::File::open(&j.path).await?;
        loop {
            let delay = r
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(retry_after)
                .unwrap_or(0.0);
            if delay > 0.0 {
                return Err(remote_error(
                    "YouTube requested an upload pause",
                    true,
                    delay,
                ));
            }
            let offset = upload_offset(
                r.headers().get("range").and_then(|v| v.to_str().ok()),
                j.size,
            )?;
            self.store.progress(&j.id, offset)?;
            f.seek(std::io::SeekFrom::Start(offset)).await?;
            let mut buf = vec![0; CHUNK.min((j.size - offset) as usize)];
            f.read_exact(&mut buf).await?;
            let end = offset + buf.len() as u64 - 1;
            r = self
                .request(
                    Method::PUT,
                    url,
                    vec![
                        ("Content-Type", "application/octet-stream".into()),
                        ("Content-Length", buf.len().to_string()),
                        ("Content-Range", format!("bytes {offset}-{end}/{}", j.size)),
                    ],
                    None,
                    Some(buf),
                )
                .await?;
            if r.status().as_u16() != 308 {
                return completion(r).await;
            }
            let next = upload_offset(
                r.headers().get("range").and_then(|v| v.to_str().ok()),
                j.size,
            )?;
            if next <= offset {
                return Err(remote_error("YouTube made no upload progress", true, 0.0));
            }
        }
    }
    pub async fn schedule(&self, j: &Job) -> Result<bool> {
        let id = j.video_id.as_ref().context("Video ID missing")?;
        let v: Value = self
            .request(
                Method::GET,
                &format!("{}?part=status&id={id}", self.endpoints.api),
                vec![],
                None,
                None,
            )
            .await?
            .json()
            .await?;
        let status = v["items"]
            .as_array()
            .and_then(|v| v.first())
            .and_then(|v| v.get("status"))
            .context("Uploaded video not visible to this account; check YouTube Studio")?;
        if matches!(
            status["uploadStatus"].as_str(),
            Some("failed" | "rejected" | "deleted")
        ) {
            bail!("YouTube could not process this video. Check YouTube Studio.");
        }
        if schedule_matches(status, j.publish_at) || status["privacyStatus"] == "public" {
            return Ok(true);
        }
        if status["privacyStatus"] != "private" {
            bail!("Video privacy changed outside Release Room. Check YouTube Studio.");
        }
        if j.publish_at <= now() + 60.0 {
            return Ok(false);
        }
        let mut mutable = json!({"privacyStatus":"private","publishAt":iso(j.publish_at)});
        for key in [
            "license",
            "embeddable",
            "publicStatsViewable",
            "selfDeclaredMadeForKids",
            "containsSyntheticMedia",
        ] {
            if let Some(v) = status.get(key) {
                mutable[key] = v.clone();
            }
        }
        let r: Value = self
            .request(
                Method::PUT,
                &format!("{}?part=status", self.endpoints.api),
                vec![],
                Some(json!({"id":id,"status":mutable})),
                None,
            )
            .await?
            .json()
            .await?;
        if !schedule_matches(&r["status"], j.publish_at) {
            return Err(remote_error(
                "YouTube did not confirm publication time. Check YouTube Studio.",
                true,
                0.0,
            ));
        }
        Ok(true)
    }
    pub async fn connect(
        &self,
        config: OAuthClient,
        code: &str,
        redirect: &str,
        verifier: &str,
    ) -> Result<String> {
        let r = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", redirect),
                ("grant_type", "authorization_code"),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Cannot reach Google authorization server"))?;
        if !r.status().is_success() {
            bail!("Google authorization failed. Try connecting again.");
        }
        let v: Value = r.json().await?;
        let c = Credentials {
            token: v["access_token"]
                .as_str()
                .context("Google returned no token")?
                .into(),
            refresh_token: v["refresh_token"]
                .as_str()
                .context("Google did not grant offline access; reconnect with consent")?
                .into(),
            client_id: config.client_id,
            client_secret: config.client_secret,
            expires_at: now() + v["expires_in"].as_f64().unwrap_or(3600.0),
            scopes: v["scope"]
                .as_str()
                .unwrap_or("")
                .split_whitespace()
                .map(String::from)
                .collect(),
        };
        if !c.scopes.iter().any(|s| s == SCOPE) {
            bail!("The YouTube scheduling permission was not granted");
        }
        let response = self
            .http
            .get("https://www.googleapis.com/youtube/v3/channels?part=snippet&mine=true")
            .bearer_auth(&c.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Cannot verify YouTube channel"))?;
        if !response.status().is_success() {
            bail!("Could not verify the connected YouTube channel");
        }
        let channel: Value = response.json().await?;
        let item = channel["items"]
            .as_array()
            .and_then(|v| v.first())
            .context("This Google account has no YouTube channel")?;
        let id = item["id"].as_str().context("Channel ID missing")?;
        let name = item["snippet"]["title"]
            .as_str()
            .unwrap_or("YouTube channel");
        let mut guard = self.credentials.lock().await;
        if let Some(old) = self.store.setting("channel_id")?
            && old != id
        {
            bail!(
                "This queue belongs to a different channel. Reconnect the original channel or use another data directory."
            );
        }
        // Legacy Python credentials have no channel binding: avoid changing them with pending jobs.
        if self.store.setting("channel_id")?.is_none()
            && guard.is_some()
            && self
                .store
                .list()?
                .iter()
                .any(|j| !matches!(j.state, Status::Scheduled | Status::Cancelled))
        {
            bail!(
                "Legacy queue has pending jobs. Finish them with existing credentials before connecting a new web OAuth client."
            );
        }
        self.store.set("channel_id", id)?;
        self.store.set("channel_name", name)?;
        save_credentials(&self.store.dir.join("token.json"), &c)?;
        *guard = Some(c);
        Ok(name.into())
    }
}
async fn completion(r: Response) -> Result<String> {
    let v: Value = r.json().await.map_err(|_| {
        remote_error(
            "Invalid upload completion response; will reconcile",
            true,
            0.0,
        )
    })?;
    let id = v["id"]
        .as_str()
        .filter(|v| {
            !v.is_empty()
                && v.len() < 128
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .ok_or_else(|| remote_error("YouTube returned no video ID; will reconcile", true, 0.0))?;
    Ok(id.into())
}
pub fn validate_session(s: &str) -> Result<()> {
    let u = reqwest::Url::parse(s)?;
    if u.scheme() != "https"
        || u.host_str() != Some("www.googleapis.com")
        || u.port_or_known_default() != Some(443)
        || !u.username().is_empty()
        || u.password().is_some()
        || !u.path().starts_with("/upload/youtube/")
    {
        bail!("Unexpected upload session URL");
    }
    Ok(())
}
pub fn upload_offset(range: Option<&str>, total: u64) -> Result<u64> {
    let offset = match range {
        None => 0,
        Some(v) => v
            .strip_prefix("bytes=0-")
            .context("Invalid upload offset")?
            .parse::<u64>()?
            .checked_add(1)
            .context("Invalid upload offset")?,
    };
    if offset >= total {
        return Err(remote_error("Invalid incomplete upload offset", true, 0.0));
    }
    Ok(offset)
}
pub fn retry_after(s: &str) -> f64 {
    if let Ok(v) = s.parse::<f64>() {
        return if v.is_finite() { v.max(0.0) } else { 0.0 };
    }
    chrono::DateTime::parse_from_rfc2822(s)
        .map(|d| (d.timestamp() as f64 - now()).max(0.0))
        .unwrap_or(0.0)
}
pub fn schedule_matches(v: &Value, t: f64) -> bool {
    v["publishAt"]
        .as_str()
        .and_then(|s| parse_time(s).ok())
        .is_some_and(|s| (s - t).abs() < 1.0)
}
