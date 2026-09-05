use super::*;
use crate::{model::*, store::Store};

#[test]
fn a_queued_job_starts_clean_and_is_listed() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 1024);
    assert_eq!(job.state, Status::Queued);
    assert_eq!(job.attempts, 0);
    assert_eq!(job.bytes_sent, 0);
    assert_eq!(job.size, 1024);
    assert!(job.session_url.is_none() && job.video_id.is_none());
    assert_eq!(f.store.list().unwrap().len(), 1);
    assert_eq!(f.store.get(&job.id).unwrap().title, "A demo");
    assert!(f.store.get("missing").is_err());
}

#[test]
fn invalid_metadata_and_empty_files_never_reach_the_queue() {
    let f = Fixture::new();
    let path = f.video("sample.mp4", 16);
    let bad = Metadata {
        title: String::new(),
        ..metadata(now(), now() + HOUR)
    };
    assert!(f.store.add(&path, 16, "digest", bad).is_err());
    let m = metadata(now(), now() + HOUR);
    assert!(f.store.add(&path, 0, "digest", m).is_err());
    assert!(f.store.list().unwrap().is_empty());
}

#[test]
fn claim_takes_only_due_work_in_upload_order() {
    let f = Fixture::new();
    let late = f.job("late.mp4", 16);
    f.store.force(&late.id, "upload_at", now() + HOUR).unwrap();
    f.store
        .force(&late.id, "next_attempt", now() + HOUR)
        .unwrap();
    let early = f.job("early.mp4", 16);

    let claimed = f.store.claim(now()).unwrap().expect("due job");
    assert_eq!(claimed.id, early.id);
    assert_eq!(claimed.state, Status::Uploading);
    assert_eq!(claimed.attempts, 1);
    // The uploading job is no longer claimable, and the later one is not yet due.
    assert!(f.store.claim(now()).unwrap().is_none());
    assert_eq!(
        f.store.claim(now() + HOUR).unwrap().map(|j| j.id),
        Some(late.id)
    );
}

#[test]
fn terminal_and_cancelled_states_are_never_claimed() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 16);
    for state in ["scheduled", "failed", "missed", "cancelled", "uploading"] {
        f.store.force(&job.id, "state", state).unwrap();
        assert!(
            f.store.claim(now()).unwrap().is_none(),
            "claimed a {state} job"
        );
    }
    f.store.force(&job.id, "state", "retry").unwrap();
    assert!(f.store.claim(now()).unwrap().is_some());
}

#[test]
fn restart_returns_interrupted_uploads_to_retry() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 16);
    f.store.claim(now()).unwrap().unwrap();
    assert_eq!(f.store.get(&job.id).unwrap().state, Status::Uploading);

    // A genuinely new connection over the same durable state, as after a crash.
    let restarted = Store::open(&f.dir.path().join("data")).unwrap();
    restarted.recover().unwrap();
    let recovered = restarted.get(&job.id).unwrap();
    assert_eq!(recovered.state, Status::Retry);
    assert_eq!(recovered.attempts, 1, "recovery does not spend an attempt");
    assert!(restarted.claim(now()).unwrap().is_some());
}

#[test]
fn progress_and_remote_identity_are_persisted() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 1024);
    f.store.session(&job.id, "https://example.test/s").unwrap();
    f.store.progress(&job.id, 512).unwrap();
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.bytes_sent, 512);
    assert_eq!(saved.session_url.as_deref(), Some("https://example.test/s"));

    f.store.video(&job.id, "video_123").unwrap();
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.video_id.as_deref(), Some("video_123"));
    assert_eq!(saved.bytes_sent, saved.size, "a finished upload reads 100%");
}

#[test]
fn cancellation_is_refused_once_an_upload_exists() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 16);
    f.store.session(&job.id, "https://example.test/s").unwrap();
    let error = f.store.cancel(&job.id).unwrap_err().to_string();
    assert!(error.contains("YouTube Studio"), "{error}");

    let other = f.job("other.mp4", 16);
    f.store.video(&other.id, "video_123").unwrap();
    assert!(f.store.cancel(&other.id).is_err());

    let scheduled = f.job("third.mp4", 16);
    f.store.force(&scheduled.id, "state", "scheduled").unwrap();
    assert!(f.store.cancel(&scheduled.id).is_err());
}

#[test]
fn cancellation_works_from_every_state_without_an_upload() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 16);
    for state in ["queued", "retry", "failed", "missed"] {
        f.store.force(&job.id, "state", state).unwrap();
        f.store.cancel(&job.id).unwrap();
        assert_eq!(f.store.get(&job.id).unwrap().state, Status::Cancelled);
    }
    // Cancelling twice is refused rather than silently accepted.
    assert!(f.store.cancel(&job.id).is_err());
}

#[test]
fn retry_moves_the_deadline_and_keeps_the_remote_identity() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 16);
    f.store.session(&job.id, "https://example.test/s").unwrap();
    f.store.video(&job.id, "video_123").unwrap();
    f.store
        .finish(&job.id, Status::Failed, Some("boom"), now())
        .unwrap();
    f.store.force(&job.id, "attempts", 8).unwrap();

    let deadline = now() + 2.0 * HOUR;
    f.store.retry(&job.id, deadline).unwrap();
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Retry);
    assert_eq!(saved.publish_at, deadline);
    assert_eq!(saved.attempts, 0);
    assert!(saved.last_error.is_none());
    assert_eq!(saved.session_url.as_deref(), Some("https://example.test/s"));
    assert_eq!(saved.video_id.as_deref(), Some("video_123"));
}

#[test]
fn retry_refuses_stale_deadlines_and_settled_jobs() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 16);
    f.store.force(&job.id, "state", "failed").unwrap();
    for deadline in [now(), now() + 30.0, f64::NAN] {
        assert!(f.store.retry(&job.id, deadline).is_err());
    }
    for state in ["queued", "uploading", "scheduled", "cancelled"] {
        f.store.force(&job.id, "state", state).unwrap();
        assert!(
            f.store.retry(&job.id, now() + HOUR).is_err(),
            "retried a {state} job"
        );
    }
}

#[test]
fn settings_round_trip_and_overwrite() {
    let f = Fixture::new();
    assert!(f.store.setting("channel_id").unwrap().is_none());
    f.store.set("channel_id", "UC123").unwrap();
    f.store.set("channel_name", "Studio").unwrap();
    f.store.set("channel_id", "UC456").unwrap();
    assert_eq!(
        f.store.setting("channel_id").unwrap().as_deref(),
        Some("UC456")
    );
    assert_eq!(
        f.store.setting("channel_name").unwrap().as_deref(),
        Some("Studio")
    );
}

#[test]
fn a_queue_without_the_progress_column_is_migrated_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    // The pre-0.2 schema, written by the retired Python CLI.
    let legacy = rusqlite::Connection::open(data.join("queue.sqlite3")).unwrap();
    legacy
        .execute_batch(
            "CREATE TABLE jobs (
               id TEXT PRIMARY KEY,path TEXT NOT NULL,size INTEGER NOT NULL,sha256 TEXT NOT NULL,
               title TEXT NOT NULL,description TEXT NOT NULL,tags TEXT NOT NULL,category TEXT NOT NULL,
               made_for_kids INTEGER NOT NULL,synthetic_media INTEGER NOT NULL,
               upload_at REAL NOT NULL,publish_at REAL NOT NULL,state TEXT NOT NULL DEFAULT 'queued',
               attempts INTEGER NOT NULL DEFAULT 0,next_attempt REAL NOT NULL,session_url TEXT,
               video_id TEXT,last_error TEXT,created_at REAL NOT NULL,updated_at REAL NOT NULL);
             INSERT INTO jobs VALUES ('old','/tmp/old.mp4',7,'digest','Old','',   '[]','22',0,0,
               0,0,'queued',0,0,NULL,NULL,NULL,0,0);",
        )
        .unwrap();
    drop(legacy);

    let store = Store::open(&data).unwrap();
    let job = store.get("old").unwrap();
    assert_eq!(job.bytes_sent, 0);
    assert_eq!(job.title, "Old");
    // Reopening an already-migrated queue leaves it alone.
    Store::open(&data).unwrap().get("old").unwrap();
}
