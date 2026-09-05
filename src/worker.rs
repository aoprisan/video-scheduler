use crate::{
    model::*,
    store::Store,
    youtube::{RemoteError, YouTube},
};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::watch;

pub trait UploadService: Send + Sync {
    fn start(&self, j: &Job) -> impl std::future::Future<Output = Result<String>> + Send;
    fn upload(&self, j: &Job) -> impl std::future::Future<Output = Result<String>> + Send;
    fn schedule(&self, j: &Job) -> impl std::future::Future<Output = Result<bool>> + Send;
}
impl UploadService for YouTube {
    async fn start(&self, j: &Job) -> Result<String> {
        self.start(j).await
    }
    async fn upload(&self, j: &Job) -> Result<String> {
        self.upload(j).await
    }
    async fn schedule(&self, j: &Job) -> Result<bool> {
        self.schedule(j).await
    }
}
pub async fn process(store: &Store, yt: &impl UploadService, mut j: Job) -> Result<()> {
    let result: Result<Status> = async {
        if j.video_id.is_none() {
            if j.publish_at <= now() + 60.0 {
                return Ok(Status::Missed);
            }
            let path = j.path.clone();
            let (size, digest) =
                tokio::task::spawn_blocking(move || fingerprint(std::path::Path::new(&path)))
                    .await??;
            if size != j.size || digest != j.sha256 {
                anyhow::bail!(
                    "Video file changed since scheduling. Restore it or create a new job."
                );
            }
            if j.session_url.is_none() {
                let url = yt.start(&j).await?;
                store.session(&j.id, &url)?;
                j.session_url = Some(url);
            }
            let id = yt.upload(&j).await?;
            store.video(&j.id, &id)?;
            j.video_id = Some(id);
        }
        if yt.schedule(&j).await? {
            Ok(Status::Scheduled)
        } else {
            Ok(Status::Missed)
        }
    }
    .await;
    match result {
        Ok(status) => {
            let error = if status == Status::Missed {
                Some(
                    "Publication deadline missed. Uploaded video stays private unless a previous schedule was already accepted. Check Studio before retrying.",
                )
            } else {
                None
            };
            store.finish(&j.id, status, error, now())?;
        }
        Err(e) => {
            let remote = e.downcast_ref::<RemoteError>();
            let retry = remote.is_some_and(|e| e.retryable) && j.attempts < 8;
            let delay = remote
                .map_or(0.0, |e| e.delay)
                .max((30.0 * 2f64.powi((j.attempts.saturating_sub(1).min(16)) as i32)).min(3600.0));
            // Never include reqwest's URL-bearing error chains in persisted errors or logs.
            let message = if e.downcast_ref::<reqwest::Error>().is_some() {
                "Remote response could not be read".into()
            } else {
                e.to_string()
            };
            store.finish(
                &j.id,
                if retry { Status::Retry } else { Status::Failed },
                Some(&message),
                now() + delay,
            )?;
            tracing::warn!(job=%j.id,"Upload attempt did not complete");
        }
    }
    Ok(())
}
pub async fn run(
    store: Arc<Store>,
    yt: Arc<YouTube>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    store.recover()?;
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        if yt.connected().await {
            if let Some(j) = store.claim(now())? {
                process(&store, yt.as_ref(), j).await?;
                continue;
            }
        }
        tokio::select! {_ = tokio::time::sleep(std::time::Duration::from_secs(5))=>{},_ = shutdown.changed()=>{return Ok(())}}
    }
}
