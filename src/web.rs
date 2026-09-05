use crate::{
    model::*,
    store::Store,
    views,
    youtube::{self, YouTube},
};
use anyhow::{Context, Result, bail};
use axum::{
    Form, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{io::AsyncWriteExt, sync::Mutex};
use uuid::Uuid;

pub struct Pending {
    verifier: String,
    expires: f64,
}
pub struct App {
    pub store: Arc<Store>,
    pub youtube: Arc<YouTube>,
    pub csrf: String,
    pub public_url: String,
    pub password: Option<String>,
    pub client_path: PathBuf,
    pub pending: Mutex<HashMap<String, Pending>>,
}
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/jobs/new", get(new_job))
        .route("/jobs/{id}", get(detail))
        .route("/connection", get(connection))
        .route("/auth/start", post(auth_start))
        .route("/auth/callback", get(auth_callback))
        .route("/api/jobs", get(list).post(upload))
        .route("/api/jobs/{id}/cancel", post(cancel))
        .route("/api/jobs/{id}/retry", post(retry))
        .route("/media/{id}", get(media))
        .route("/assets/app.css", get(css))
        .route("/assets/app.js", get(js))
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024 * 1024usize))
        .layer(middleware::from_fn_with_state(app.clone(), protect))
        .with_state(app)
}
async fn protect(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let expected = reqwest::Url::parse(&app.public_url).unwrap();
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let authority = &app.public_url[app.public_url.find("://").unwrap() + 3..];
    if host != authority {
        return (StatusCode::FORBIDDEN, "Unexpected host").into_response();
    }
    if let Some(password) = &app.password {
        let actual = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Basic "))
            .and_then(|s| STANDARD.decode(s).ok())
            .unwrap_or_default();
        let expected = Sha256::digest(format!("studio:{password}"));
        let actual = Sha256::digest(actual);
        let difference = expected
            .iter()
            .zip(actual)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        if difference != 0 {
            return (
                StatusCode::UNAUTHORIZED,
                [(
                    header::WWW_AUTHENTICATE,
                    "Basic realm=\"Release Room\", charset=\"UTF-8\"",
                )],
                "Studio credentials required",
            )
                .into_response();
        }
    }
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok());
        if origin != Some(expected.origin().ascii_serialization().as_str()) {
            return (StatusCode::FORBIDDEN, "Invalid request origin").into_response();
        }
        if req.uri().path().starts_with("/api/")
            && req
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                != Some(&app.csrf)
        {
            return (StatusCode::FORBIDDEN, "Refresh this page before submitting").into_response();
        }
    }
    let mut response = next.run(req).await;
    for (key, value) in [
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob:; media-src 'self' blob:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    ] {
        response
            .headers_mut()
            .insert(key, HeaderValue::from_static(value));
    }
    response
}
#[derive(Deserialize, Default)]
struct Filter {
    filter: Option<String>,
}
async fn index(State(app): State<Arc<App>>, Query(q): Query<Filter>) -> Response {
    match app.store.list() {
        Ok(jobs) => Html(views::queue(
            &app,
            &jobs,
            app.youtube.connected().await,
            app.store.setting("channel_name").ok().flatten().as_deref(),
            q.filter.as_deref().unwrap_or("all"),
        ))
        .into_response(),
        Err(_) => server_error(&app),
    }
}
async fn new_job(State(app): State<Arc<App>>) -> Html<String> {
    Html(views::schedule(&app))
}
async fn detail(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    match app.store.get(&id) {
        Ok(j) => Html(views::detail(&app, &j)).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Html(views::error(&app, "Video not found")),
        )
            .into_response(),
    }
}
async fn connection(State(app): State<Arc<App>>) -> Html<String> {
    Html(views::connection(
        &app,
        app.youtube.connected().await,
        app.store.setting("channel_name").ok().flatten().as_deref(),
        youtube::load_client(&app.client_path).is_ok(),
    ))
}
async fn list(State(app): State<Arc<App>>) -> Response {
    match app.store.list() {
        Ok(jobs) => Json(jobs).into_response(),
        Err(_) => server_error(&app),
    }
}
fn server_error(app: &App) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Html(views::error(
            app,
            "Could not read the queue. Check server logs and disk availability.",
        )),
    )
        .into_response()
}
fn api_error(e: anyhow::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":e.to_string()})),
    )
        .into_response()
}
async fn upload(State(app): State<Arc<App>>, multipart: Multipart) -> Response {
    match receive_upload(&app, multipart).await {
        Ok(j) => (StatusCode::CREATED, Json(json!({"id":j.id}))).into_response(),
        Err(e) => api_error(e),
    }
}
async fn receive_upload(app: &App, mut multipart: Multipart) -> Result<Job> {
    let media = app.store.dir.join("media");
    std::fs::create_dir_all(&media)?;
    let mut temporary: Option<tempfile::NamedTempFile> = None;
    let mut fields = HashMap::new();
    let mut digest = String::new();
    let mut size = 0u64;
    let mut extension = String::new();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .context("Could not read file upload")?
    {
        let name = field.name().unwrap_or("").to_owned();
        if name == "video" {
            if temporary.is_some() {
                bail!("Choose only one video");
            }
            extension = std::path::Path::new(field.file_name().unwrap_or(""))
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !["mp4", "mov", "webm", "mkv", "m4v", "avi"].contains(&extension.as_str()) {
                bail!("Choose an MP4, MOV, WebM, MKV, M4V, or AVI video");
            }
            let temp = tempfile::NamedTempFile::new_in(&media)?;
            let mut file = tokio::fs::File::from_std(temp.as_file().try_clone()?);
            let mut hasher = Sha256::new();
            while let Some(chunk) = field
                .chunk()
                .await
                .context("Video transfer was interrupted")?
            {
                size += chunk.len() as u64;
                if size > 256 * 1024 * 1024 * 1024 {
                    bail!("Video exceeds 256 GB");
                }
                file.write_all(&chunk).await?;
                hasher.update(&chunk);
            }
            file.flush().await?;
            file.sync_all().await?;
            digest = format!("{:x}", hasher.finalize());
            temporary = Some(temp);
        } else {
            let mut value = Vec::new();
            while let Some(chunk) = field.chunk().await? {
                if value.len() + chunk.len() > 16_384 {
                    bail!("Form field is too long");
                }
                value.extend_from_slice(&chunk);
            }
            fields.insert(name, String::from_utf8(value)?);
        }
    }
    let field = |key: &str| fields.get(key).map(String::as_str).unwrap_or("");
    let m = Metadata {
        title: field("title").into(),
        description: field("description").into(),
        tags: field("tags")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        category: field("category").into(),
        made_for_kids: match field("made_for_kids") {
            "true" => true,
            "false" => false,
            _ => bail!("Choose the video's audience"),
        },
        synthetic_media: field("synthetic_media") == "true",
        upload_at: if field("upload_at").is_empty() {
            now()
        } else {
            parse_time(field("upload_at"))?
        },
        publish_at: parse_time(field("publish_at"))
            .context("Choose a publication time with a timezone")?,
    };
    m.validate(now())?;
    if size == 0 {
        bail!("Choose a nonempty video file");
    }
    let temp = temporary.context("Choose a video file")?;
    let path = media.join(format!("{}.{}", Uuid::new_v4(), extension));
    temp.persist(&path)?;
    match app.store.add(&path, size, &digest, m) {
        Ok(j) => Ok(j),
        Err(e) => {
            let _ = std::fs::remove_file(path);
            Err(e)
        }
    }
}
async fn cancel(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    match app.store.cancel(&id) {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => api_error(e),
    }
}
#[derive(Deserialize)]
struct Retry {
    publish_at: String,
}
async fn retry(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(input): Json<Retry>,
) -> Response {
    match parse_time(&input.publish_at).and_then(|t| app.store.retry(&id, t)) {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => api_error(e),
    }
}
async fn media(State(app): State<Arc<App>>, Path(id): Path<String>, req: Request) -> Response {
    use tower::ServiceExt;
    match app.store.get(&id) {
        Ok(j) => tower_http::services::ServeFile::new(j.path)
            .oneshot(req)
            .await
            .unwrap()
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Body::empty()).into_response(),
    }
}
async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/app.css"),
    )
}
async fn js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
}
#[derive(Deserialize)]
struct Csrf {
    csrf: String,
}
async fn auth_start(State(app): State<Arc<App>>, Form(form): Form<Csrf>) -> Response {
    if form.csrf != app.csrf {
        return (StatusCode::FORBIDDEN, "Refresh the page and try again").into_response();
    }
    let config = match youtube::load_client(&app.client_path) {
        Ok(c) => c,
        Err(e) => return Html(views::error(&app, &e.to_string())).into_response(),
    };
    let state = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut pending = app.pending.lock().await;
    pending.retain(|_, v| v.expires > now());
    if pending.len() >= 16 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many pending connections. Try in ten minutes.",
        )
            .into_response();
    }
    pending.insert(
        state.clone(),
        Pending {
            verifier,
            expires: now() + 600.0,
        },
    );
    let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", &format!("{}/auth/callback", app.public_url)),
        ("response_type", "code"),
        ("scope", youtube::SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("state", state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ]);
    let mut response = Redirect::to(url.as_str()).into_response();
    let secure = if app.public_url.starts_with("https:") {
        "; Secure"
    } else {
        ""
    };
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "release_oauth={state}; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=600{secure}"
        ))
        .unwrap(),
    );
    response
}
#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
async fn auth_callback(
    State(app): State<Arc<App>>,
    Query(q): Query<Callback>,
    headers: axum::http::HeaderMap,
) -> Response {
    let state = q.state.unwrap_or_default();
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|s| s.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .map(str::trim)
                .find_map(|v| v.strip_prefix("release_oauth="))
        });
    if state.is_empty() || cookie != Some(state.as_str()) {
        return (
            StatusCode::FORBIDDEN,
            Html(views::error(
                &app,
                "Invalid connection session. Start again from YouTube connection.",
            )),
        )
            .into_response();
    }
    let pending = app.pending.lock().await.remove(&state);
    let Some(p) = pending.filter(|p| p.expires > now()) else {
        return Html(views::error(
            &app,
            "Connection session expired. Start again.",
        ))
        .into_response();
    };
    let result = async {
        if q.error.is_some() {
            bail!("YouTube connection was not authorized");
        }
        let config = youtube::load_client(&app.client_path)?;
        app.youtube
            .connect(
                config,
                q.code
                    .as_deref()
                    .context("Google returned no authorization code")?,
                &format!("{}/auth/callback", app.public_url),
                &p.verifier,
            )
            .await
    }
    .await;
    let mut response=match result{Ok(_)=>Redirect::to("/connection").into_response(),Err(_)=>Html(views::error(&app,"Could not connect this channel. Check the OAuth client, permissions, and that you selected the queue's original channel, then try again.")).into_response()};
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("release_oauth=; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    response
}
