//! Tests run against temporary data directories and a simulated YouTube peer.
//! They exercise queue timing, private upload, scheduling, retry and restart
//! recovery, cancellation, validation, and the web layer without contacting
//! Google or publishing content.
mod model;
mod store;
mod web;
mod worker;
mod youtube;

use crate::{model::*, store::Store};
use std::{path::PathBuf, sync::Arc};

pub const HOUR: f64 = 3600.0;

/// A store in a temporary directory. The handle keeps the directory alive.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub store: Arc<Store>,
}
impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("data")).unwrap());
        Self { dir, store }
    }
    /// Writes a video file of `len` pseudo-random bytes and returns its path.
    pub fn video(&self, name: &str, len: usize) -> PathBuf {
        let path = self.dir.path().join(name);
        let bytes: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        std::fs::write(&path, bytes).unwrap();
        path
    }
    /// Queues a job for a fresh video file, publishing an hour out.
    pub fn job(&self, name: &str, len: usize) -> Job {
        let path = self.video(name, len);
        let (size, digest) = fingerprint(&path).unwrap();
        self.store
            .add(&path, size, &digest, metadata(now(), now() + HOUR))
            .unwrap()
    }
}
pub fn metadata(upload_at: f64, publish_at: f64) -> Metadata {
    Metadata {
        title: "A demo".into(),
        description: "Description".into(),
        tags: vec!["demo".into()],
        category: "22".into(),
        made_for_kids: false,
        synthetic_media: true,
        upload_at,
        publish_at,
    }
}
