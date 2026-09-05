use super::*;
use crate::{
    model::*,
    worker,
    youtube::{Credentials, Endpoints, RemoteError, SCOPE, YouTube, *},
};
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A stateful HTTP peer standing in for YouTube. It owns the received bytes and
/// the remote video status, so restarts and lost responses can be replayed
/// against durable remote state rather than a mock's expectations.
#[derive(Default)]
struct Peer {
    calls: Vec<(String, String)>,
    ranges: Vec<String>,
    received: Vec<u8>,
    total: u64,
    status: Value,
    /// Status codes returned instead of handling the next requests, newest last.
    failures: Vec<(u16, Option<String>)>,
    /// Accept at most this many bytes per chunk, as a server truncating a write.
    accept_limit: Option<usize>,
    session: String,
    token_calls: usize,
}
impl Peer {
    fn count(&self, method: &str, path: &str) -> usize {
        self.calls
            .iter()
            .filter(|(m, p)| m == method && p == path)
            .count()
    }
}
type Shared = Arc<Mutex<Peer>>;

struct Fake {
    peer: Shared,
    endpoints: Endpoints,
    _task: tokio::task::JoinHandle<()>,
}
async fn peer() -> Fake {
    let shared: Shared = Default::default();
    let app = Router::new()
        .route("/upload/videos", post(begin))
        .route("/upload/videos/session", put(chunk))
        .route("/videos", get(status).put(update))
        .route("/token", post(token))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    shared.lock().unwrap().session = format!("http://{addr}/upload/videos/session");
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Fake {
        peer: shared,
        endpoints: Endpoints {
            api: format!("http://{addr}/videos"),
            upload: format!("http://{addr}/upload/videos"),
            token: format!("http://{addr}/token"),
        },
        _task: task,
    }
}
/// Returns a queued failure, letting a test script transient and permanent errors.
fn scripted(peer: &mut Peer) -> Option<Response> {
    let (code, retry_after) = peer.failures.pop()?;
    let mut response = (StatusCode::from_u16(code).unwrap(), "{}").into_response();
    if let Some(v) = retry_after {
        response
            .headers_mut()
            .insert("retry-after", v.parse().unwrap());
    }
    Some(response)
}
async fn begin(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let mut p = s.lock().unwrap();
    p.calls.push(("POST".into(), "/upload/videos".into()));
    if let Some(r) = scripted(&mut p) {
        return r;
    }
    let sent: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(sent["status"]["privacyStatus"], "private");
    assert!(
        sent["status"].get("publishAt").is_none(),
        "the first upload must not carry a publication time"
    );
    p.total = headers["x-upload-content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    p.status = sent["status"].clone();
    p.status["uploadStatus"] = json!("uploaded");
    let session = p.session.clone();
    (
        StatusCode::OK,
        [(header::LOCATION, session)],
        axum::Json(json!({})),
    )
        .into_response()
}
fn incomplete(p: &Peer) -> Response {
    let mut response = (StatusCode::PERMANENT_REDIRECT, "{}").into_response();
    if !p.received.is_empty() {
        response.headers_mut().insert(
            "range",
            format!("bytes=0-{}", p.received.len() - 1).parse().unwrap(),
        );
    }
    response
}
async fn chunk(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let mut p = s.lock().unwrap();
    p.calls
        .push(("PUT".into(), "/upload/videos/session".into()));
    if let Some(r) = scripted(&mut p) {
        return r;
    }
    let range = headers["content-range"].to_str().unwrap().to_owned();
    p.ranges.push(range.clone());
    if !body.is_empty() {
        let start: u64 = range[6..range.find('-').unwrap()].parse().unwrap();
        assert_eq!(
            start,
            p.received.len() as u64,
            "chunk resumed at the wrong offset"
        );
        let take = p.accept_limit.unwrap_or(body.len()).min(body.len());
        p.received.extend_from_slice(&body[..take]);
    }
    if p.received.len() as u64 != p.total {
        return incomplete(&p);
    }
    let status = p.status.clone();
    axum::Json(json!({"id":"video_123","status":status})).into_response()
}
async fn status(State(s): State<Shared>) -> Response {
    let mut p = s.lock().unwrap();
    p.calls.push(("GET".into(), "/videos".into()));
    if let Some(r) = scripted(&mut p) {
        return r;
    }
    let status = p.status.clone();
    let items = if status.is_null() {
        json!([])
    } else {
        json!([{"id":"video_123","status":status}])
    };
    axum::Json(json!({"items":items})).into_response()
}
async fn update(State(s): State<Shared>, body: Bytes) -> Response {
    let mut p = s.lock().unwrap();
    p.calls.push(("PUT".into(), "/videos".into()));
    if let Some(r) = scripted(&mut p) {
        return r;
    }
    let sent: Value = serde_json::from_slice(&body).unwrap();
    for (k, v) in sent["status"].as_object().unwrap() {
        p.status[k] = v.clone();
    }
    let status = p.status.clone();
    axum::Json(json!({"id":"video_123","status":status})).into_response()
}
async fn token(State(s): State<Shared>) -> Response {
    let mut p = s.lock().unwrap();
    p.token_calls += 1;
    axum::Json(json!({"access_token":"fresh","expires_in":3600})).into_response()
}

fn connect(f: &Fixture, expires_at: f64, scopes: &[&str]) {
    let credentials = Credentials {
        token: "stale".into(),
        refresh_token: "refresh".into(),
        client_id: "client".into(),
        client_secret: "secret".into(),
        expires_at,
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
    };
    save_credentials(&f.store.dir.join("token.json"), &credentials).unwrap();
}
/// A connected client whose endpoints point at the fake peer.
fn client(f: &Fixture, fake: &Fake) -> YouTube {
    connect(f, now() + HOUR, &[SCOPE]);
    YouTube::with_endpoints(
        f.store.clone(),
        Endpoints {
            api: fake.endpoints.api.clone(),
            upload: fake.endpoints.upload.clone(),
            token: fake.endpoints.token.clone(),
        },
    )
    .unwrap()
}
fn error(e: anyhow::Error) -> (String, bool) {
    let remote = e.downcast_ref::<RemoteError>().expect("a remote error");
    (remote.message.clone(), remote.retryable)
}

#[tokio::test]
async fn a_whole_job_uploads_privately_and_takes_a_publication_time() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 4096);
    let yt = client(&f, &fake);

    worker::process(&f.store, &yt, f.store.claim(now()).unwrap().unwrap())
        .await
        .unwrap();

    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Scheduled);
    assert_eq!(saved.video_id.as_deref(), Some("video_123"));
    let p = fake.peer.lock().unwrap();
    assert_eq!(p.received, std::fs::read(&job.path).unwrap(), "bytes match");
    assert_eq!(
        p.status["privacyStatus"], "private",
        "never made public here"
    );
    assert!((parse_time(p.status["publishAt"].as_str().unwrap()).unwrap() - job.publish_at) < 1.0);
    assert_eq!(p.status["containsSyntheticMedia"], true);
    assert_eq!(p.count("POST", "/upload/videos"), 1);
}

#[tokio::test]
async fn an_interrupted_upload_resumes_at_the_received_offset() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 4096);
    let yt = client(&f, &fake);

    // The session exists and 1024 bytes arrived before the worker stopped.
    let session = yt.start(&job).await.unwrap();
    f.store.session(&job.id, &session).unwrap();
    {
        let mut p = fake.peer.lock().unwrap();
        let bytes = std::fs::read(&job.path).unwrap();
        p.received.extend_from_slice(&bytes[..1024]);
    }
    let job = f.store.get(&job.id).unwrap();

    assert_eq!(yt.upload(&job).await.unwrap(), "video_123");
    let p = fake.peer.lock().unwrap();
    assert_eq!(p.received, std::fs::read(&job.path).unwrap());
    let sent = p.ranges.iter().find(|r| !r.contains('*')).unwrap();
    assert!(sent.starts_with("bytes 1024-"), "resumed at {sent}");
    assert_eq!(p.count("POST", "/upload/videos"), 1, "no second session");
    assert_eq!(f.store.get(&job.id).unwrap().bytes_sent, 1024);
}

#[tokio::test]
async fn a_partially_accepted_chunk_is_continued_from_the_new_offset() {
    let f = Fixture::new();
    let fake = peer().await;
    fake.peer.lock().unwrap().accept_limit = Some(1000);
    let job = f.job("sample.mp4", 4096);
    let yt = client(&f, &fake);

    let session = yt.start(&job).await.unwrap();
    f.store.session(&job.id, &session).unwrap();
    let job = f.store.get(&job.id).unwrap();
    assert_eq!(yt.upload(&job).await.unwrap(), "video_123");

    let p = fake.peer.lock().unwrap();
    assert_eq!(p.received, std::fs::read(&job.path).unwrap());
    let sent: Vec<_> = p.ranges.iter().filter(|r| !r.contains('*')).collect();
    assert!(sent.len() >= 4, "expected several chunks, got {sent:?}");
}

#[tokio::test]
async fn an_expired_upload_session_is_reported_as_permanent() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);
    let yt = client(&f, &fake);
    let session = yt.start(&job).await.unwrap();
    f.store.session(&job.id, &session).unwrap();
    fake.peer.lock().unwrap().failures = vec![(410, None)];

    let job = f.store.get(&job.id).unwrap();
    let (message, retryable) = error(yt.upload(&job).await.unwrap_err());
    assert!(message.contains("Upload session expired"), "{message}");
    assert!(message.contains("YouTube Studio"));
    assert!(
        !retryable,
        "a lost session must never start a duplicate upload"
    );
}

#[tokio::test]
async fn server_errors_are_retryable_and_client_errors_are_not() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);
    let yt = client(&f, &fake);

    for (code, expected) in [
        (503, true),
        (429, true),
        (500, true),
        (403, false),
        (400, false),
    ] {
        fake.peer.lock().unwrap().failures = vec![(code, None)];
        let (message, retryable) = error(yt.start(&job).await.unwrap_err());
        assert_eq!(retryable, expected, "{code}: {message}");
        assert!(message.contains(&code.to_string()), "{message}");
    }
}

#[tokio::test]
async fn a_requested_pause_is_carried_into_the_retry_delay() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);
    let yt = client(&f, &fake);
    fake.peer.lock().unwrap().failures = vec![(429, Some("120".into()))];

    let e = yt.start(&job).await.unwrap_err();
    let remote = e.downcast_ref::<RemoteError>().unwrap();
    assert!(remote.retryable);
    assert_eq!(remote.delay, 120.0);
}

#[tokio::test]
async fn an_already_scheduled_video_is_reconciled_without_a_second_request() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);
    let yt = client(&f, &fake);
    // As after a lost response: YouTube already holds the publication time.
    fake.peer.lock().unwrap().status = json!({
        "privacyStatus":"private","uploadStatus":"uploaded","publishAt":iso(job.publish_at)});
    f.store.video(&job.id, "video_123").unwrap();

    let job = f.store.get(&job.id).unwrap();
    assert!(yt.schedule(&job).await.unwrap());
    assert_eq!(
        fake.peer.lock().unwrap().count("PUT", "/videos"),
        0,
        "an accepted schedule is not sent twice"
    );
}

#[tokio::test]
async fn an_already_public_video_counts_as_scheduled() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);
    let yt = client(&f, &fake);
    fake.peer.lock().unwrap().status = json!({"privacyStatus":"public","uploadStatus":"processed"});
    f.store.video(&job.id, "video_123").unwrap();

    assert!(yt.schedule(&f.store.get(&job.id).unwrap()).await.unwrap());
    assert_eq!(fake.peer.lock().unwrap().count("PUT", "/videos"), 0);
}

#[tokio::test]
async fn scheduling_stops_when_the_video_changed_outside_release_room() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);
    let yt = client(&f, &fake);
    f.store.video(&job.id, "video_123").unwrap();
    let job = f.store.get(&job.id).unwrap();

    fake.peer.lock().unwrap().status =
        json!({"privacyStatus":"unlisted","uploadStatus":"uploaded"});
    let e = yt.schedule(&job).await.unwrap_err().to_string();
    assert!(e.contains("privacy changed outside"), "{e}");

    fake.peer.lock().unwrap().status = json!({"privacyStatus":"private","uploadStatus":"rejected"});
    let e = yt.schedule(&job).await.unwrap_err().to_string();
    assert!(e.contains("could not process"), "{e}");

    fake.peer.lock().unwrap().status = Value::Null;
    let e = yt.schedule(&job).await.unwrap_err().to_string();
    assert!(e.contains("not visible to this account"), "{e}");
}

#[tokio::test]
async fn a_deadline_reached_before_scheduling_reports_a_miss() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);
    let yt = client(&f, &fake);
    fake.peer.lock().unwrap().status = json!({"privacyStatus":"private","uploadStatus":"uploaded"});
    f.store.video(&job.id, "video_123").unwrap();
    f.store.force(&job.id, "publish_at", now()).unwrap();

    assert!(!yt.schedule(&f.store.get(&job.id).unwrap()).await.unwrap());
    let p = fake.peer.lock().unwrap();
    assert_eq!(p.count("PUT", "/videos"), 0);
    assert!(p.status.get("publishAt").is_none(), "stays private");
}

#[tokio::test]
async fn an_expired_access_token_is_refreshed_once_before_use() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);
    connect(&f, 0.0, &[SCOPE]);
    let yt = YouTube::with_endpoints(
        f.store.clone(),
        Endpoints {
            api: fake.endpoints.api.clone(),
            upload: fake.endpoints.upload.clone(),
            token: fake.endpoints.token.clone(),
        },
    )
    .unwrap();

    yt.start(&job).await.unwrap();
    yt.start(&job).await.unwrap();
    assert_eq!(fake.peer.lock().unwrap().token_calls, 1, "refreshed once");
    // The refreshed credentials are persisted for the next process.
    let saved: Value =
        serde_json::from_slice(&std::fs::read(f.store.dir.join("token.json")).unwrap()).unwrap();
    assert_eq!(saved["token"], "fresh");
}

#[tokio::test]
async fn uploading_needs_a_connection_that_granted_the_scheduling_scope() {
    let f = Fixture::new();
    let fake = peer().await;
    let job = f.job("sample.mp4", 512);

    let yt = YouTube::new(f.store.clone()).unwrap();
    assert!(!yt.connected().await);
    assert!(yt.access_token().await.is_err());

    connect(
        &f,
        now() + HOUR,
        &["https://www.googleapis.com/auth/youtube.upload"],
    );
    let yt = client(&f, &fake);
    *yt.credentials.lock().await = Some(Credentials {
        scopes: vec!["https://www.googleapis.com/auth/youtube.upload".into()],
        expires_at: now() + HOUR,
        ..Default::default()
    });
    let e = yt.start(&job).await.unwrap_err().to_string();
    assert!(e.contains("Reconnect YouTube"), "{e}");
}

#[test]
fn only_google_upload_sessions_are_accepted() {
    validate_session("https://www.googleapis.com/upload/youtube/v3/videos?upload_id=x").unwrap();
    for bad in [
        "http://www.googleapis.com/upload/youtube/v3/videos",
        "https://evil.test/upload/youtube/v3/videos",
        "https://user:pass@www.googleapis.com/upload/youtube/v3/videos",
        "https://www.googleapis.com/youtube/v3/videos",
        "https://www.googleapis.com:8443/upload/youtube/v3/videos",
        "not a url",
    ] {
        assert!(validate_session(bad).is_err(), "accepted {bad}");
    }
}

#[test]
fn the_resume_offset_follows_the_range_header() {
    assert_eq!(upload_offset(None, 100).unwrap(), 0);
    assert_eq!(upload_offset(Some("bytes=0-49"), 100).unwrap(), 50);
    // A complete or nonsensical range must not be treated as an offset.
    assert!(upload_offset(Some("bytes=0-99"), 100).is_err());
    assert!(upload_offset(Some("bytes=10-49"), 100).is_err());
    assert!(upload_offset(Some("garbage"), 100).is_err());
    assert!(upload_offset(Some(&format!("bytes=0-{}", u64::MAX)), 100).is_err());
}

#[test]
fn retry_after_accepts_seconds_and_dates() {
    assert_eq!(retry_after("120"), 120.0);
    assert_eq!(retry_after("-5"), 0.0);
    assert_eq!(retry_after("nonsense"), 0.0);
    assert_eq!(retry_after("NaN"), 0.0);
    assert_eq!(
        retry_after("Wed, 01 Jan 2020 00:00:00 GMT"),
        0.0,
        "past dates"
    );
    let future = chrono::Utc::now() + chrono::Duration::seconds(300);
    let delay = retry_after(&future.to_rfc2822());
    assert!((240.0..=300.0).contains(&delay), "{delay}");
}

#[test]
fn a_publication_time_matches_within_a_second() {
    let t = 1893456000.0;
    assert!(schedule_matches(
        &json!({"publishAt":"2030-01-01T00:00:00Z"}),
        t
    ));
    assert!(schedule_matches(
        &json!({"publishAt":"2030-01-01T01:00:00+01:00"}),
        t
    ));
    assert!(!schedule_matches(
        &json!({"publishAt":"2030-01-01T00:01:00Z"}),
        t
    ));
    assert!(!schedule_matches(&json!({}), t));
    assert!(!schedule_matches(&json!({"publishAt":"soon"}), t));
}

#[test]
fn the_oauth_client_must_be_a_web_application() {
    let f = Fixture::new();
    let path = f.dir.path().join("client_secret.json");
    assert!(load_client(&path).is_err(), "missing file");
    std::fs::write(
        &path,
        br#"{"installed":{"client_id":"a","client_secret":"b"}}"#,
    )
    .unwrap();
    let e = load_client(&path).err().unwrap().to_string();
    assert!(e.contains("Web application"), "{e}");
    std::fs::write(&path, br#"{"web":{"client_id":"a","client_secret":"b"}}"#).unwrap();
    assert_eq!(load_client(&path).unwrap().client_id, "a");
}

#[test]
fn credentials_are_written_atomically_and_read_back() {
    let f = Fixture::new();
    let path = f.store.dir.join("token.json");
    let credentials = Credentials {
        token: "t".into(),
        refresh_token: "r".into(),
        scopes: vec![SCOPE.into()],
        ..Default::default()
    };
    save_credentials(&path, &credentials).unwrap();
    save_credentials(&path, &credentials).unwrap();
    let read: Credentials = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(read.refresh_token, "r");
    // Tokens written by the retired Python CLI still load, extra fields and all,
    // so an existing data directory keeps working until the channel is reconnected.
    std::fs::write(
        &path,
        br#"{"token":"t","refresh_token":"r","token_uri":"https://oauth2.googleapis.com/token",
             "client_id":"c","client_secret":"s","scopes":["https://www.googleapis.com/auth/youtube.force-ssl"],
             "universe_domain":"googleapis.com","account":"","expiry":"2030-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let legacy: Credentials = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(legacy.token, "t");
    assert_eq!(legacy.client_id, "c");
    assert_eq!(legacy.scopes, [SCOPE]);
    assert_eq!(
        legacy.expires_at, 0.0,
        "an unknown expiry forces a refresh first"
    );
}
