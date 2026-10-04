use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use tagos_mail_assistant::errors::AssistantError;
use tagos_mail_assistant::model::{
    AssistantEvent, Followup, FollowupFilter, FollowupStatus, Prefs,
};
use tagos_mail_assistant::store::FollowupStore;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS followups (
    account_key TEXT NOT NULL,
    id TEXT NOT NULL,
    address TEXT NOT NULL,
    thread_id TEXT,
    status TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (account_key, id)
);
CREATE TABLE IF NOT EXISTS events (
    account_key TEXT NOT NULL,
    id TEXT NOT NULL,
    followup_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    delivered_at TEXT,
    body TEXT NOT NULL,
    PRIMARY KEY (account_key, id)
);
CREATE TABLE IF NOT EXISTS prefs (
    account_key TEXT PRIMARY KEY,
    body TEXT NOT NULL
);
";

#[derive(Clone)]
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    pub fn memory() -> Result<Self, AssistantError> {
        Self::from_connection(Connection::open_in_memory().map_err(map_db)?)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, AssistantError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|_| AssistantError::BackendUnavailable)?;
            }
        }
        Self::from_connection(Connection::open(path).map_err(map_db)?)
    }

    pub fn open_default() -> Result<Self, AssistantError> {
        Self::open(PathBuf::from("data/demo-followups.db"))
    }

    fn from_connection(conn: Connection) -> Result<Self, AssistantError> {
        conn.execute_batch(SCHEMA).map_err(map_db)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, AssistantError> {
        self.conn
            .lock()
            .map_err(|_| AssistantError::BackendUnavailable)
    }
}

#[async_trait]
impl FollowupStore for SqliteStore {
    async fn insert_followup(&self, followup: Followup) -> Result<(), AssistantError> {
        let conn = self.lock()?;
        let body =
            serde_json::to_string(&followup).map_err(|_| AssistantError::BackendUnavailable)?;
        conn.execute(
            "INSERT INTO followups (account_key, id, address, thread_id, status, updated_at, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                followup.account_key,
                followup.id,
                followup.counterpart_address,
                followup.thread_id,
                followup.status.as_str(),
                followup.updated_at.to_rfc3339(),
                body,
            ],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_followup(
        &self,
        account_key: &str,
        id: &str,
    ) -> Result<Option<Followup>, AssistantError> {
        let conn = self.lock()?;
        let body: Option<String> = conn
            .query_row(
                "SELECT body FROM followups WHERE account_key = ?1 AND id = ?2",
                (account_key, id),
                |row| row.get(0),
            )
            .optional()
            .map_err(map_db)?;
        body.map(|raw| decode(&raw)).transpose()
    }

    async fn list_followups(
        &self,
        account_key: &str,
        filter: FollowupFilter,
    ) -> Result<Vec<Followup>, AssistantError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT body FROM followups WHERE account_key = ?1")
            .map_err(map_db)?;
        let mut rows = Vec::new();
        let iter = stmt
            .query_map([account_key], |row| row.get::<_, String>(0))
            .map_err(map_db)?;
        for raw in iter {
            let followup = decode::<Followup>(&raw.map_err(map_db)?)?;
            if matches_filter(followup.status, filter) {
                rows.push(followup);
            }
        }
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(rows)
    }

    async fn find_active(
        &self,
        account_key: &str,
        address: &str,
        thread_id: Option<&str>,
    ) -> Result<Option<Followup>, AssistantError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT body, status, address, thread_id FROM followups WHERE account_key = ?1",
            )
            .map_err(map_db)?;
        let iter = stmt
            .query_map([account_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(map_db)?;
        for row in iter {
            let (body, status, stored_address, stored_thread) = row.map_err(map_db)?;
            if !is_active_status(&status) {
                continue;
            }
            if !stored_address.eq_ignore_ascii_case(address) {
                continue;
            }
            if stored_thread.as_deref() != thread_id {
                continue;
            }
            return Ok(Some(decode(&body)?));
        }
        Ok(None)
    }

    async fn transition(
        &self,
        account_key: &str,
        id: &str,
        expect: FollowupStatus,
        followup: Followup,
        event: Option<AssistantEvent>,
    ) -> Result<bool, AssistantError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(map_db)?;
        let current: Option<String> = tx
            .query_row(
                "SELECT status FROM followups WHERE account_key = ?1 AND id = ?2",
                (account_key, id),
                |row| row.get(0),
            )
            .optional()
            .map_err(map_db)?;
        let Some(current) = current else {
            return Err(AssistantError::NotFound);
        };
        if current != expect.as_str() {
            return Ok(false);
        }
        let body =
            serde_json::to_string(&followup).map_err(|_| AssistantError::BackendUnavailable)?;
        tx.execute(
            "UPDATE followups
             SET address = ?3, thread_id = ?4, status = ?5, updated_at = ?6, body = ?7
             WHERE account_key = ?1 AND id = ?2",
            rusqlite::params![
                account_key,
                id,
                followup.counterpart_address,
                followup.thread_id,
                followup.status.as_str(),
                followup.updated_at.to_rfc3339(),
                body,
            ],
        )
        .map_err(map_db)?;
        if let Some(event) = event {
            let event_body =
                serde_json::to_string(&event).map_err(|_| AssistantError::BackendUnavailable)?;
            tx.execute(
                "INSERT INTO events (account_key, id, followup_id, created_at, delivered_at, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    event.account_key,
                    event.id,
                    event.followup_id,
                    event.created_at.to_rfc3339(),
                    event.delivered_at.map(|at| at.to_rfc3339()),
                    event_body,
                ],
            )
            .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(true)
    }

    async fn undelivered_events(
        &self,
        account_key: &str,
    ) -> Result<Vec<AssistantEvent>, AssistantError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT body FROM events
                 WHERE account_key = ?1 AND delivered_at IS NULL
                 ORDER BY created_at, id",
            )
            .map_err(map_db)?;
        let iter = stmt
            .query_map([account_key], |row| row.get::<_, String>(0))
            .map_err(map_db)?;
        let mut rows = Vec::new();
        for raw in iter {
            rows.push(decode(&raw.map_err(map_db)?)?);
        }
        Ok(rows)
    }

    async fn mark_event_delivered(
        &self,
        account_key: &str,
        event_id: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AssistantError> {
        let conn = self.lock()?;
        let body: Option<String> = conn
            .query_row(
                "SELECT body FROM events WHERE account_key = ?1 AND id = ?2",
                (account_key, event_id),
                |row| row.get(0),
            )
            .optional()
            .map_err(map_db)?;
        let Some(raw) = body else {
            return Err(AssistantError::NotFound);
        };
        let mut event = decode::<AssistantEvent>(&raw)?;
        if event.delivered_at.is_none() {
            event.delivered_at = Some(at);
            let encoded =
                serde_json::to_string(&event).map_err(|_| AssistantError::BackendUnavailable)?;
            conn.execute(
                "UPDATE events SET delivered_at = ?3, body = ?4
                 WHERE account_key = ?1 AND id = ?2 AND delivered_at IS NULL",
                rusqlite::params![account_key, event_id, at.to_rfc3339(), encoded],
            )
            .map_err(map_db)?;
        }
        Ok(())
    }

    async fn prefs(&self, account_key: &str) -> Result<Prefs, AssistantError> {
        let conn = self.lock()?;
        let body: Option<String> = conn
            .query_row(
                "SELECT body FROM prefs WHERE account_key = ?1",
                [account_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_db)?;
        match body {
            Some(raw) => decode(&raw),
            None => Ok(Prefs::new(account_key)),
        }
    }

    async fn put_prefs(&self, prefs: Prefs) -> Result<(), AssistantError> {
        let conn = self.lock()?;
        let body = serde_json::to_string(&prefs).map_err(|_| AssistantError::BackendUnavailable)?;
        conn.execute(
            "INSERT INTO prefs (account_key, body) VALUES (?1, ?2)
             ON CONFLICT(account_key) DO UPDATE SET body = excluded.body",
            rusqlite::params![prefs.account_key, body],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn forget(&self, account_key: &str) -> Result<u64, AssistantError> {
        let conn = self.lock()?;
        let deleted = conn
            .execute(
                "DELETE FROM followups WHERE account_key = ?1",
                [account_key],
            )
            .map_err(map_db)? as u64;
        conn.execute("DELETE FROM events WHERE account_key = ?1", [account_key])
            .map_err(map_db)?;
        conn.execute("DELETE FROM prefs WHERE account_key = ?1", [account_key])
            .map_err(map_db)?;
        Ok(deleted)
    }

    async fn open_followups(&self) -> Result<Vec<Followup>, AssistantError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT body FROM followups WHERE status IN ('waiting', 'overdue', 'snoozed')")
            .map_err(map_db)?;
        let iter = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_db)?;
        let mut rows = Vec::new();
        for raw in iter {
            rows.push(decode(&raw.map_err(map_db)?)?);
        }
        Ok(rows)
    }

    async fn purge_closed_before(&self, cutoff: DateTime<Utc>) -> Result<u64, AssistantError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(map_db)?;
        let mut stmt = tx
            .prepare("SELECT id, body FROM followups")
            .map_err(map_db)?;
        let iter = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(map_db)?;
        let mut doomed = Vec::new();
        for row in iter {
            let (id, raw) = row.map_err(map_db)?;
            let followup = decode::<Followup>(&raw)?;
            if followup.status.is_closed() && followup.updated_at < cutoff {
                doomed.push(id);
            }
        }
        drop(stmt);
        for id in &doomed {
            tx.execute("DELETE FROM followups WHERE id = ?1", [id])
                .map_err(map_db)?;
            tx.execute("DELETE FROM events WHERE followup_id = ?1", [id])
                .map_err(map_db)?;
        }
        tx.commit().map_err(map_db)?;
        Ok(doomed.len() as u64)
    }
}

fn matches_filter(status: FollowupStatus, filter: FollowupFilter) -> bool {
    match filter {
        FollowupFilter::Active => status.is_active(),
        FollowupFilter::Replied => status == FollowupStatus::Replied,
        FollowupFilter::All => true,
    }
}

fn is_active_status(status: &str) -> bool {
    matches!(status, "waiting" | "overdue" | "snoozed")
}

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, AssistantError> {
    serde_json::from_str(raw).map_err(|_| AssistantError::BackendUnavailable)
}

fn map_db(err: rusqlite::Error) -> AssistantError {
    match &err {
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            AssistantError::InvalidArgument("duplicate followup".into())
        }
        _ => AssistantError::BackendUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[tokio::test]
    async fn sqlite_store_matches_contract() {
        let store = SqliteStore::memory().unwrap();
        tagos_mail_assistant::store_contract_suite(store).await;
    }

    #[tokio::test]
    async fn sqlite_store_keeps_followups_after_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "tagos-mail-assistant-{}",
            tagos_mail_assistant::model::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("followups.db");
        let at = Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap();
        let followup = Followup {
            id: "fu-keep".into(),
            account_key: "demo".into(),
            counterpart_address: "priya@example.com".into(),
            counterpart_name: Some("Priya Shah".into()),
            thread_id: None,
            about: Some("contract".into()),
            created_at: at,
            due_at: at + chrono::Duration::days(4),
            tz: "America/New_York".into(),
            baseline_at: at,
            status: FollowupStatus::Waiting,
            snooze_until: None,
            replied_message_id: None,
            replied_at: None,
            draft_id: None,
            last_checked_at: None,
            updated_at: at,
        };
        {
            let store = SqliteStore::open(&path).unwrap();
            store.insert_followup(followup.clone()).await.unwrap();
        }
        let reopened = SqliteStore::open(&path).unwrap();
        let loaded = reopened
            .get_followup("demo", "fu-keep")
            .await
            .unwrap()
            .expect("row survives reopen");
        assert_eq!(loaded, followup);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
