use super::*;
use crate::{
    model::*,
    web::{App, router},
    youtube::YouTube,
};
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const ORIGIN: &str = "http://localhost:3000";
const HOST: &str = "localhost:3000";
const CSRF: &str = "test-csrf-token";

fn app(f: &Fixture, password: Option<&str>) -> Arc<App> {
    Arc::new(App {
        store: f.store.clone(),
        youtube: Arc::new(YouTube::new(f.store.clone()).unwrap()),
        csrf: CSRF.into(),
        public_url: ORIGIN.into(),
        password: password.map(str::to_string),
        client_path: f.store.dir.join("client_secret.json"),
        pending: Default::default(),
    })
}
async fn send(f: &Fixture, request: Request<Body>) -> (StatusCode, String) {
    call(app(f, None), request).await
}
async fn call(app: Arc<App>, request: Request<Body>) -> (StatusCode, String) {
    let response = router(app).oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}
fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap()
}
fn post(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, ORIGIN)
        .header("x-csrf-token", CSRF)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}
/// A multipart submission of the scheduling form.
fn form(fields: &[(&str, &str)], file: Option<(&str, &[u8])>) -> Request<Body> {
    let boundary = "----releaseroomtest";
    let mut body: Vec<u8> = vec![];
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    if let Some((filename, bytes)) = file {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"video\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri("/api/jobs")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, ORIGIN)
        .header("x-csrf-token", CSRF)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}
fn fields(publish_at: f64) -> Vec<(String, String)> {
    vec![
        ("title".into(), "A demo".to_string()),
        ("description".into(), "Description".into()),
        ("tags".into(), "demo, gameplay".into()),
        ("category".into(), "22".into()),
        ("made_for_kids".into(), "false".into()),
        ("synthetic_media".into(), "true".into()),
        ("upload_at".into(), String::new()),
        ("publish_at".into(), iso(publish_at)),
    ]
}
fn borrowed(v: &[(String, String)]) -> Vec<(&str, &str)> {
    v.iter().map(|(k, x)| (k.as_str(), x.as_str())).collect()
}

#[tokio::test]
async fn the_queue_page_renders_empty_and_then_lists_videos() {
    let f = Fixture::new();
    let (status, body) = send(&f, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Your next release starts here"),
        "empty state"
    );
    assert!(
        body.contains("Connect your YouTube channel"),
        "not connected"
    );

    let job = f.job("sample.mp4", 256);
    let (_, body) = send(&f, get("/")).await;
    assert!(body.contains("A demo"));
    assert!(body.contains(&iso(job.publish_at)));
}

#[tokio::test]
async fn the_queue_filters_by_state() {
    let f = Fixture::new();
    let queued = f.job("queued.mp4", 64);
    let done = f.job("done.mp4", 64);
    f.store.force(&done.id, "title", "Already out").unwrap();
    f.store.force(&done.id, "state", "scheduled").unwrap();
    f.store.force(&queued.id, "title", "Still waiting").unwrap();

    let (_, all) = send(&f, get("/")).await;
    assert!(all.contains("Still waiting") && all.contains("Already out"));
    let (_, upcoming) = send(&f, get("/?filter=upcoming")).await;
    assert!(upcoming.contains("Still waiting") && !upcoming.contains("Already out"));
    let (_, scheduled) = send(&f, get("/?filter=scheduled")).await;
    assert!(!scheduled.contains("Still waiting") && scheduled.contains("Already out"));
    let (_, attention) = send(&f, get("/?filter=attention")).await;
    assert!(!attention.contains("Still waiting"));
}

#[tokio::test]
async fn the_detail_page_shows_the_schedule_and_missing_videos_are_reported() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    let (status, body) = send(&f, get(&format!("/jobs/{}", job.id))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("A demo") && body.contains("Description"));
    assert!(
        body.contains(&format!("/media/{}", job.id)),
        "plays the file"
    );

    let (status, body) = send(&f, get("/jobs/does-not-exist")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Video not found"));
}

#[tokio::test]
async fn the_scheduling_form_queues_a_video_and_stores_its_own_copy() {
    let f = Fixture::new();
    let bytes = b"a small but complete video file".to_vec();
    let values = fields(now() + HOUR);
    let (status, body) = send(&f, form(&borrowed(&values), Some(("clip.MP4", &bytes)))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let job = f.store.list().unwrap().pop().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["id"], job.id);
    assert_eq!(job.title, "A demo");
    assert_eq!(job.tags, ["demo", "gameplay"]);
    assert!(job.synthetic_media && !job.made_for_kids);
    assert_eq!(job.size, bytes.len() as u64);
    assert!(job.upload_at <= now() + 1.0, "uploads on the next pass");
    // The queue owns its copy, so the browser's file can move or change.
    let stored = std::path::Path::new(&job.path);
    assert!(stored.starts_with(f.store.dir.join("media")));
    assert_eq!(std::fs::read(stored).unwrap(), bytes);
    assert_eq!(fingerprint(stored).unwrap().1, job.sha256);
}

/// Asserts a submission is refused with a message the browser can show.
async fn rejected(
    f: &Fixture,
    values: &[(String, String)],
    file: Option<(&str, &[u8])>,
    expected: &str,
) {
    let (status, body) = send(f, form(&borrowed(values), file)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains(expected), "expected {expected:?} in {body}");
}

#[tokio::test]
async fn the_form_rejects_bad_submissions_without_keeping_the_file() {
    let f = Fixture::new();
    let bytes = b"video".to_vec();
    let ok = fields(now() + HOUR);

    rejected(&f, &ok, None, "video file").await;
    rejected(&f, &ok, Some(("clip.exe", &bytes)), "MP4").await;
    rejected(&f, &ok, Some(("clip.mp4", b"")), "nonempty").await;
    rejected(
        &f,
        &fields(now() + 30.0),
        Some(("clip.mp4", &bytes)),
        "60 seconds",
    )
    .await;

    let mut missing_title = ok.clone();
    missing_title[0].1 = String::new();
    rejected(&f, &missing_title, Some(("clip.mp4", &bytes)), "Title").await;

    let mut no_audience = ok.clone();
    no_audience[4].1 = String::new();
    rejected(&f, &no_audience, Some(("clip.mp4", &bytes)), "audience").await;

    let mut bad_date = ok.clone();
    bad_date[7].1 = "tomorrow".into();
    rejected(&f, &bad_date, Some(("clip.mp4", &bytes)), "timezone").await;

    assert!(f.store.list().unwrap().is_empty());
    let media = f.store.dir.join("media");
    let kept = std::fs::read_dir(&media).map(|d| d.count()).unwrap_or(0);
    assert_eq!(kept, 0, "a rejected upload leaves no file behind");
}

#[tokio::test]
async fn the_api_cancels_and_retries_jobs() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    let (status, _) = send(&f, post(&format!("/api/jobs/{}/cancel", job.id), "{}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(f.store.get(&job.id).unwrap().state, Status::Cancelled);

    // A cancelled job cannot be retried, and the reason reaches the browser.
    let body = format!(r#"{{"publish_at":"{}"}}"#, iso(now() + 2.0 * HOUR));
    let (status, response) = send(&f, post(&format!("/api/jobs/{}/retry", job.id), &body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        response.contains("failed, missed, or retrying"),
        "{response}"
    );

    let failed = f.job("other.mp4", 64);
    f.store.force(&failed.id, "state", "failed").unwrap();
    let (status, _) = send(&f, post(&format!("/api/jobs/{}/retry", failed.id), &body)).await;
    assert_eq!(status, StatusCode::OK);
    let saved = f.store.get(&failed.id).unwrap();
    assert_eq!(saved.state, Status::Retry);
    assert!((saved.publish_at - (now() + 2.0 * HOUR)).abs() < 2.0);

    // A stale deadline is refused rather than silently accepted.
    let stale = format!(r#"{{"publish_at":"{}"}}"#, iso(now()));
    let (status, _) = send(&f, post(&format!("/api/jobs/{}/retry", failed.id), &stale)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_json_queue_omits_credentials_and_local_paths() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    f.store
        .session(&job.id, "https://example.test/secret")
        .unwrap();

    let (status, body) = send(&f, get("/api/jobs")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("secret"), "session URLs stay server-side");
    assert!(!body.contains(&job.path), "local paths stay server-side");
    assert!(!body.contains(&job.sha256));
    let jobs: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(jobs[0]["title"], "A demo");
    assert_eq!(jobs[0]["state"], "queued");
}

#[tokio::test]
async fn requests_for_another_host_are_refused() {
    let f = Fixture::new();
    let request = Request::builder()
        .uri("/")
        .header(header::HOST, "evil.test")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&f, request).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn writes_need_a_matching_origin_and_the_page_token() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    let path = format!("/api/jobs/{}/cancel", job.id);

    let mut request = post(&path, "{}");
    request.headers_mut().remove(header::ORIGIN);
    assert_eq!(send(&f, request).await.0, StatusCode::FORBIDDEN);

    let mut request = post(&path, "{}");
    request
        .headers_mut()
        .insert(header::ORIGIN, "http://evil.test".parse().unwrap());
    assert_eq!(send(&f, request).await.0, StatusCode::FORBIDDEN);

    let mut request = post(&path, "{}");
    request
        .headers_mut()
        .insert("x-csrf-token", "wrong".parse().unwrap());
    let (status, body) = send(&f, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("Refresh this page"));

    assert_eq!(f.store.get(&job.id).unwrap().state, Status::Queued);
    assert_eq!(send(&f, post(&path, "{}")).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_password_gates_every_page() {
    let f = Fixture::new();
    let app = app(&f, Some("a-sufficiently-long-studio-password"));

    let (status, _) = call(app.clone(), get("/")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let mut request = get("/");
    let encoded = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .encode("studio:a-sufficiently-long-studio-password")
    };
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Basic {encoded}").parse().unwrap(),
    );
    assert_eq!(call(app.clone(), request).await.0, StatusCode::OK);

    let mut request = get("/");
    let wrong = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode("studio:wrong")
    };
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Basic {wrong}").parse().unwrap(),
    );
    assert_eq!(call(app, request).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn every_response_carries_the_hardening_headers() {
    let f = Fixture::new();
    let response = router(app(&f, None)).oneshot(get("/")).await.unwrap();
    let headers = response.headers();
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["cache-control"], "no-store");
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("frame-ancestors 'none'"));
}

#[tokio::test]
async fn the_connection_page_explains_the_one_time_setup() {
    let f = Fixture::new();
    let (status, body) = send(&f, get("/connection")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("One-time setup"), "no OAuth client yet");
    assert!(
        body.contains("http://localhost:3000/auth/callback"),
        "{body}"
    );

    std::fs::write(
        f.store.dir.join("client_secret.json"),
        br#"{"web":{"client_id":"a","client_secret":"b"}}"#,
    )
    .unwrap();
    let (_, body) = send(&f, get("/connection")).await;
    assert!(body.contains("Connect YouTube"));
    assert!(!body.contains("One-time setup"));
}

#[tokio::test]
async fn an_unsigned_oauth_callback_is_refused() {
    let f = Fixture::new();
    let (status, body) = send(&f, get("/auth/callback?code=stolen&state=guessed")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("Invalid connection session"));
}

#[tokio::test]
async fn stored_media_is_served_for_playback() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 128);
    let (status, body) = send(&f, get(&format!("/media/{}", job.id))).await;
    assert_eq!(status, StatusCode::OK);
    let expected = std::fs::read(&job.path).unwrap();
    assert_eq!(
        body,
        String::from_utf8_lossy(&expected),
        "serves the stored file"
    );
    assert_eq!(
        send(&f, get("/media/missing")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_static_assets_are_served_from_the_binary() {
    let f = Fixture::new();
    let (status, body) = send(&f, get("/assets/app.css")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("--accent"));
    let (status, body) = send(&f, get("/assets/app.js")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("csrf"));
}
