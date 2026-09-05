use super::*;
use crate::{
    model::*,
    worker::{self, UploadService},
    youtube::remote_error,
};
use std::sync::Mutex;

const SESSION: &str = "https://www.googleapis.com/upload/youtube/v3/videos?upload_id=sensitive";

/// A stateful stand-in for YouTube: it owns the remote video identity across
/// worker restarts, so recovery can be observed without HTTP.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<&'static str>>,
    /// Fails the next `upload` after the bytes were accepted, as a lost response would.
    lose_upload_response: Mutex<bool>,
    /// Errors returned instead of a successful schedule, one per call.
    schedule_errors: Mutex<Vec<anyhow::Error>>,
    /// Reports the deadline as missed rather than accepted.
    deadline_missed: Mutex<bool>,
}
impl Fake {
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
    fn count(&self, name: &str) -> usize {
        self.calls().iter().filter(|c| **c == name).count()
    }
}
impl UploadService for Fake {
    async fn start(&self, _j: &Job) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push("start");
        Ok(SESSION.into())
    }
    async fn upload(&self, _j: &Job) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push("upload");
        if std::mem::take(&mut *self.lose_upload_response.lock().unwrap()) {
            return Err(remote_error("Network request failed", true, 0.0));
        }
        Ok("video_123".into())
    }
    async fn schedule(&self, _j: &Job) -> anyhow::Result<bool> {
        self.calls.lock().unwrap().push("schedule");
        if let Some(e) = self.schedule_errors.lock().unwrap().pop() {
            return Err(e);
        }
        Ok(!*self.deadline_missed.lock().unwrap())
    }
}

/// One worker pass over every job that is due at `t`, as `worker --once` does.
async fn pass(f: &Fixture, yt: &Fake, t: f64) -> usize {
    let mut processed = 0;
    while let Some(j) = f.store.claim(t).unwrap() {
        worker::process(&f.store, yt, j).await.unwrap();
        processed += 1;
    }
    processed
}

#[tokio::test]
async fn a_due_job_uploads_privately_then_takes_a_publication_time() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 2048);
    let yt = Fake::default();

    assert_eq!(pass(&f, &yt, now() - 1.0).await, 0, "not due yet");
    assert!(yt.calls().is_empty());

    assert_eq!(pass(&f, &yt, now()).await, 1);
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Scheduled);
    assert_eq!(saved.video_id.as_deref(), Some("video_123"));
    assert!(saved.last_error.is_none());
    assert_eq!(yt.calls(), ["start", "upload", "schedule"]);

    // A scheduled job is finished work; a later pass leaves it alone.
    assert_eq!(pass(&f, &yt, now() + 10.0).await, 0);
}

#[tokio::test]
async fn a_deadline_that_passed_before_uploading_sends_nothing() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    f.store.force(&job.id, "publish_at", now()).unwrap();
    let yt = Fake::default();

    pass(&f, &yt, now()).await;
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Missed);
    assert!(yt.calls().is_empty(), "no bytes leave for a missed job");
    assert!(saved.last_error.unwrap().contains("stays private"));
}

#[tokio::test]
async fn a_deadline_missed_during_upload_leaves_the_video_private() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    let yt = Fake {
        deadline_missed: Mutex::new(true),
        ..Default::default()
    };

    pass(&f, &yt, now()).await;
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Missed);
    assert_eq!(
        saved.video_id.as_deref(),
        Some("video_123"),
        "upload stands"
    );
    assert!(saved.last_error.unwrap().contains("Check Studio"));
}

#[tokio::test]
async fn a_file_changed_since_scheduling_fails_before_sending_bytes() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    std::fs::write(&job.path, b"different content entirely").unwrap();
    let yt = Fake::default();

    pass(&f, &yt, now()).await;
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Failed);
    assert!(
        saved
            .last_error
            .unwrap()
            .contains("changed since scheduling")
    );
    assert!(yt.calls().is_empty());
}

#[tokio::test]
async fn a_missing_file_fails_that_job_without_blocking_the_others() {
    let f = Fixture::new();
    let broken = f.job("gone.mp4", 64);
    let healthy = f.job("fine.mp4", 64);
    std::fs::remove_file(&broken.path).unwrap();
    let yt = Fake::default();

    assert_eq!(pass(&f, &yt, now()).await, 2);
    assert_eq!(f.store.get(&broken.id).unwrap().state, Status::Failed);
    assert_eq!(f.store.get(&healthy.id).unwrap().state, Status::Scheduled);
}

#[tokio::test]
async fn a_transient_failure_backs_off_and_retries_later() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    let yt = Fake {
        schedule_errors: Mutex::new(vec![remote_error(
            "YouTube HTTP 503: backendError",
            true,
            0.0,
        )]),
        ..Default::default()
    };

    pass(&f, &yt, now()).await;
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Retry);
    assert_eq!(saved.attempts, 1);
    assert!(
        saved.next_attempt >= now() + 25.0,
        "backs off before retrying"
    );
    assert!(saved.last_error.unwrap().contains("503"));

    // The retry is not due immediately, then succeeds when it is.
    assert_eq!(pass(&f, &yt, now()).await, 0);
    pass(&f, &yt, saved.next_attempt).await;
    assert_eq!(f.store.get(&job.id).unwrap().state, Status::Scheduled);
}

#[tokio::test]
async fn backoff_grows_with_attempts_and_honours_a_requested_delay() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    let error = || remote_error("YouTube HTTP 503: backendError", true, 0.0);
    let yt = Fake {
        schedule_errors: Mutex::new(vec![error(), error(), error()]),
        ..Default::default()
    };

    let mut delays = vec![];
    for _ in 0..3 {
        let due = f.store.get(&job.id).unwrap().next_attempt.max(now());
        pass(&f, &yt, due).await;
        // The delay is measured from the real clock the worker used, not the claim time.
        delays.push(f.store.get(&job.id).unwrap().next_attempt - now());
    }
    assert!(delays[1] > delays[0] && delays[2] > delays[1], "{delays:?}");

    // A server-requested delay wins when it is longer than the backoff.
    let other = f.job("other.mp4", 64);
    let yt = Fake {
        schedule_errors: Mutex::new(vec![remote_error("Slow down", true, 900.0)]),
        ..Default::default()
    };
    pass(&f, &yt, now()).await;
    assert!(f.store.get(&other.id).unwrap().next_attempt >= now() + 890.0);
}

#[tokio::test]
async fn a_permanent_failure_is_not_retried() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    let yt = Fake {
        schedule_errors: Mutex::new(vec![remote_error(
            "YouTube HTTP 403: forbidden",
            false,
            0.0,
        )]),
        ..Default::default()
    };

    pass(&f, &yt, now()).await;
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Failed);
    assert_eq!(saved.attempts, 1);
}

#[tokio::test]
async fn retrying_stops_after_the_eighth_attempt() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    f.store.force(&job.id, "attempts", 7).unwrap();
    let yt = Fake {
        schedule_errors: Mutex::new(vec![remote_error(
            "YouTube HTTP 503: backendError",
            true,
            0.0,
        )]),
        ..Default::default()
    };

    pass(&f, &yt, now()).await;
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.attempts, 8);
    assert_eq!(
        saved.state,
        Status::Failed,
        "the eighth attempt is the last"
    );
}

#[tokio::test]
async fn a_lost_final_upload_response_recovers_without_a_second_upload() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 512);
    let yt = Fake {
        lose_upload_response: Mutex::new(true),
        ..Default::default()
    };

    pass(&f, &yt, now()).await;
    let saved = f.store.get(&job.id).unwrap();
    assert_eq!(saved.state, Status::Retry);
    assert!(saved.video_id.is_none());
    assert_eq!(
        saved.session_url.as_deref(),
        Some(SESSION),
        "the session is kept so the upload can be reconciled"
    );

    pass(&f, &yt, saved.next_attempt).await;
    assert_eq!(f.store.get(&job.id).unwrap().state, Status::Scheduled);
    assert_eq!(yt.count("start"), 1, "no duplicate upload session");
}

#[tokio::test]
async fn rescheduling_an_uploaded_video_does_not_need_the_original_file() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    f.store.video(&job.id, "video_123").unwrap();
    f.store.force(&job.id, "state", "retry").unwrap();
    std::fs::remove_file(&job.path).unwrap();
    let yt = Fake::default();

    pass(&f, &yt, now()).await;
    assert_eq!(f.store.get(&job.id).unwrap().state, Status::Scheduled);
    assert_eq!(yt.calls(), ["schedule"], "the bytes are already at YouTube");
}

#[tokio::test]
async fn a_cancelled_job_is_never_uploaded() {
    let f = Fixture::new();
    let job = f.job("sample.mp4", 64);
    f.store.cancel(&job.id).unwrap();
    let yt = Fake::default();

    assert_eq!(pass(&f, &yt, now()).await, 0);
    assert!(yt.calls().is_empty());
    assert_eq!(f.store.get(&job.id).unwrap().state, Status::Cancelled);
}
