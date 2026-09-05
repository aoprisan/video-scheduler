use crate::model::*;
use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

pub struct Store {
    db: Mutex<Connection>,
    pub dir: PathBuf,
}
impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let dir = dir.canonicalize()?;
        let db = Connection::open(dir.join("queue.sqlite3"))?;
        db.busy_timeout(std::time::Duration::from_secs(30))?;
        db.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS jobs (
          id TEXT PRIMARY KEY,path TEXT NOT NULL,size INTEGER NOT NULL,sha256 TEXT NOT NULL,
          title TEXT NOT NULL,description TEXT NOT NULL,tags TEXT NOT NULL,category TEXT NOT NULL,
          made_for_kids INTEGER NOT NULL,synthetic_media INTEGER NOT NULL,
          upload_at REAL NOT NULL,publish_at REAL NOT NULL,state TEXT NOT NULL DEFAULT 'queued',
          attempts INTEGER NOT NULL DEFAULT 0,next_attempt REAL NOT NULL,session_url TEXT,video_id TEXT,
          last_error TEXT,created_at REAL NOT NULL,updated_at REAL NOT NULL);
        CREATE INDEX IF NOT EXISTS jobs_due ON jobs(state,next_attempt);
        CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
        let has_progress: bool = db
            .prepare("PRAGMA table_info(jobs)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .contains(&"bytes_sent".into());
        if !has_progress {
            db.execute(
                "ALTER TABLE jobs ADD COLUMN bytes_sent INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        // The process lock is held by the server before opening/recovering this queue.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("queue.sqlite3"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
        Ok(Self {
            db: Mutex::new(db),
            dir,
        })
    }
    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
        let state: String = r.get("state")?;
        let parse_err = |e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        };
        Ok(Job {
            id: r.get("id")?,
            path: r.get("path")?,
            size: r.get("size")?,
            sha256: r.get("sha256")?,
            title: r.get("title")?,
            description: r.get("description")?,
            tags: serde_json::from_str(&r.get::<_, String>("tags")?).map_err(parse_err)?,
            category: r.get("category")?,
            made_for_kids: r.get("made_for_kids")?,
            synthetic_media: r.get("synthetic_media")?,
            upload_at: r.get("upload_at")?,
            publish_at: r.get("publish_at")?,
            state: serde_json::from_value(serde_json::Value::String(state)).map_err(parse_err)?,
            attempts: r.get("attempts")?,
            next_attempt: r.get("next_attempt")?,
            session_url: r.get("session_url")?,
            video_id: r.get("video_id")?,
            last_error: r.get("last_error")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            bytes_sent: r.get("bytes_sent")?,
        })
    }
    pub fn list(&self) -> Result<Vec<Job>> {
        let db = self.db.lock().unwrap();
        Ok(db
            .prepare("SELECT * FROM jobs ORDER BY publish_at,created_at")?
            .query_map([], Self::row)?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn get(&self, id: &str) -> Result<Job> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT * FROM jobs WHERE id=?", [id], Self::row)?)
    }
    pub fn add(&self, path: &Path, size: u64, digest: &str, m: Metadata) -> Result<Job> {
        m.validate(now())?;
        if size == 0 {
            bail!("Video file is empty");
        }
        let id = Uuid::new_v4().to_string();
        let t = now();
        self.db.lock().unwrap().execute("INSERT INTO jobs(id,path,size,sha256,title,description,tags,category,made_for_kids,synthetic_media,upload_at,publish_at,next_attempt,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
          params![id,path.to_string_lossy(),size,digest,m.title,m.description,serde_json::to_string(&m.tags)?,m.category,m.made_for_kids,m.synthetic_media,m.upload_at,m.publish_at,m.upload_at,t,t])?;
        self.get(&id)
    }
    pub fn recover(&self) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE jobs SET state='retry',next_attempt=? WHERE state='uploading'",
            [now()],
        )?;
        Ok(())
    }
    pub fn claim(&self, t: f64) -> Result<Option<Job>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let id=tx.prepare("SELECT id FROM jobs WHERE state IN ('queued','retry') AND next_attempt<=? ORDER BY upload_at LIMIT 1")?.query_map([t],|r|r.get::<_,String>(0))?.next().transpose()?;
        let Some(id) = id else { return Ok(None) };
        tx.execute(
            "UPDATE jobs SET state='uploading',attempts=attempts+1,updated_at=? WHERE id=?",
            params![t, id],
        )?;
        let job = tx.query_row("SELECT * FROM jobs WHERE id=?", [id], Self::row)?;
        tx.commit()?;
        Ok(Some(job))
    }
    pub fn session(&self, id: &str, url: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE jobs SET session_url=?,updated_at=? WHERE id=?",
            params![url, now(), id],
        )?;
        Ok(())
    }
    pub fn video(&self, id: &str, video: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE jobs SET video_id=?,bytes_sent=size,updated_at=? WHERE id=?",
            params![video, now(), id],
        )?;
        Ok(())
    }
    pub fn progress(&self, id: &str, bytes: u64) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE jobs SET bytes_sent=?,updated_at=? WHERE id=?",
            params![bytes, now(), id],
        )?;
        Ok(())
    }
    pub fn finish(&self, id: &str, state: Status, error: Option<&str>, next: f64) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE jobs SET state=?,last_error=?,next_attempt=?,updated_at=? WHERE id=?",
            params![state.as_str(), error, next, now(), id],
        )?;
        Ok(())
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        let n=self.db.lock().unwrap().execute("UPDATE jobs SET state='cancelled',updated_at=? WHERE id=? AND state IN ('queued','retry','failed','missed') AND session_url IS NULL AND video_id IS NULL",params![now(),id])?;
        if n != 1 {
            bail!(
                "Only jobs without an upload session can be cancelled here. Use YouTube Studio for uploaded videos."
            );
        }
        Ok(())
    }
    pub fn retry(&self, id: &str, publish: f64) -> Result<()> {
        if !publish.is_finite() || publish <= now() + 60.0 {
            bail!("Choose a publication time over 60 seconds in the future");
        }
        let n=self.db.lock().unwrap().execute("UPDATE jobs SET state='retry',publish_at=?,attempts=0,next_attempt=?,last_error=NULL,updated_at=? WHERE id=? AND state IN ('failed','missed','retry')",params![publish,now(),now(),id])?;
        if n != 1 {
            bail!("Only failed, missed, or retrying jobs can be retried");
        }
        Ok(())
    }
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key=?", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }
    /// Test-only escape hatch for arranging states the public API refuses to create,
    /// such as an elapsed deadline or an exhausted attempt count.
    #[cfg(test)]
    pub fn force(&self, id: &str, column: &str, value: impl rusqlite::ToSql) -> Result<()> {
        let n = self.db.lock().unwrap().execute(
            &format!("UPDATE jobs SET {column}=?1 WHERE id=?2"),
            params![value, id],
        )?;
        assert_eq!(n, 1, "no such job");
        Ok(())
    }
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.lock().unwrap().execute("INSERT INTO settings(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
        Ok(())
    }
}
