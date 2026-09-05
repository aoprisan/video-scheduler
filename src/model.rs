use anyhow::{Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

pub fn now() -> f64 {
    Utc::now().timestamp_millis() as f64 / 1000.0
}
pub fn parse_time(s: &str) -> Result<f64> {
    Ok(DateTime::parse_from_rfc3339(s)?.timestamp_millis() as f64 / 1000.0)
}
pub fn iso(t: f64) -> String {
    DateTime::<Utc>::from_timestamp_millis((t * 1000.0) as i64)
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_default()
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Uploading,
    Retry,
    Scheduled,
    Failed,
    Missed,
    Cancelled,
}
impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Uploading => "uploading",
            Self::Retry => "retry",
            Self::Scheduled => "scheduled",
            Self::Failed => "failed",
            Self::Missed => "missed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn attention(&self) -> bool {
        matches!(self, Self::Retry | Self::Failed | Self::Missed)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    #[serde(skip_serializing)]
    pub path: String,
    pub size: u64,
    #[serde(skip_serializing)]
    pub sha256: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub category: String,
    pub made_for_kids: bool,
    pub synthetic_media: bool,
    pub upload_at: f64,
    pub publish_at: f64,
    pub state: Status,
    pub attempts: u32,
    pub next_attempt: f64,
    #[serde(skip_serializing)]
    pub session_url: Option<String>,
    pub video_id: Option<String>,
    pub last_error: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
    pub bytes_sent: u64,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Metadata {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub category: String,
    pub made_for_kids: bool,
    #[serde(default)]
    pub synthetic_media: bool,
    pub upload_at: f64,
    pub publish_at: f64,
}
impl Metadata {
    pub fn validate(&self, time: f64) -> Result<()> {
        if self.title.trim().is_empty()
            || self.title.chars().count() > 100
            || self.title.contains(['<', '>'])
        {
            bail!("Title must be 1–100 characters without < or >");
        }
        if self.description.len() > 5000 || self.description.contains(['<', '>']) {
            bail!("Description must be at most 5000 UTF-8 bytes without < or >");
        }
        let tag_len: usize = self
            .tags
            .iter()
            .map(|t| t.chars().count() + if t.contains(' ') { 2 } else { 0 })
            .sum();
        if self.tags.iter().any(|t| t.trim().is_empty())
            || tag_len + self.tags.len().saturating_sub(1) > 500
        {
            bail!("Tags must be nonempty and total at most 500 characters");
        }
        if self.category.parse::<u32>().unwrap_or(0) == 0 {
            bail!("Choose a valid video category");
        }
        if !self.publish_at.is_finite()
            || !self.upload_at.is_finite()
            || self.publish_at <= time.max(self.upload_at) + 60.0
        {
            bail!("Publication must be over 60 seconds after now and the upload time");
        }
        Ok(())
    }
}
pub fn fingerprint(path: &Path) -> Result<(u64, String)> {
    let mut f = std::fs::File::open(path)?;
    let before = f.metadata()?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    let mut size = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        size += n as u64;
    }
    let after = f.metadata()?;
    if size == 0 {
        bail!("Video file must not be empty");
    }
    if before.len() != after.len() || before.modified()? != after.modified()? {
        bail!("Video changed while reading");
    }
    Ok((size, format!("{:x}", hash.finalize())))
}
pub fn upload_body(j: &Job) -> Value {
    json!({"snippet":{"title":j.title,"description":j.description,"tags":j.tags,"categoryId":j.category},
      "status":{"privacyStatus":"private","selfDeclaredMadeForKids":j.made_for_kids,"containsSyntheticMedia":j.synthetic_media}})
}
