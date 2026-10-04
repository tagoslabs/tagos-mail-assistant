use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::errors::AssistantError;
use crate::model::{AssistantEvent, Followup, FollowupFilter, FollowupStatus, Prefs};

#[async_trait]
pub trait FollowupStore: Send + Sync {
    async fn insert_followup(&self, followup: Followup) -> Result<(), AssistantError>;

    async fn get_followup(
        &self,
        account_key: &str,
        id: &str,
    ) -> Result<Option<Followup>, AssistantError>;

    async fn list_followups(
        &self,
        account_key: &str,
        filter: FollowupFilter,
    ) -> Result<Vec<Followup>, AssistantError>;

    async fn find_active(
        &self,
        account_key: &str,
        address: &str,
        thread_id: Option<&str>,
    ) -> Result<Option<Followup>, AssistantError>;

    /// Apply `followup` only when the stored status is still `expect`.
    /// A lost race returns `Ok(false)` and does not write the event.
    async fn transition(
        &self,
        account_key: &str,
        id: &str,
        expect: FollowupStatus,
        followup: Followup,
        event: Option<AssistantEvent>,
    ) -> Result<bool, AssistantError>;

    async fn undelivered_events(
        &self,
        account_key: &str,
    ) -> Result<Vec<AssistantEvent>, AssistantError>;

    async fn mark_event_delivered(
        &self,
        account_key: &str,
        event_id: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AssistantError>;

    async fn prefs(&self, account_key: &str) -> Result<Prefs, AssistantError>;

    async fn put_prefs(&self, prefs: Prefs) -> Result<(), AssistantError>;

    async fn forget(&self, account_key: &str) -> Result<u64, AssistantError>;

    async fn open_followups(&self) -> Result<Vec<Followup>, AssistantError>;

    async fn purge_closed_before(&self, cutoff: DateTime<Utc>) -> Result<u64, AssistantError>;
}

#[derive(Default)]
struct Memory {
    followups: Vec<Followup>,
    events: Vec<AssistantEvent>,
    prefs: Vec<Prefs>,
}

#[derive(Clone, Default)]
pub struct InMemoryStore {
    inner: std::sync::Arc<Mutex<Memory>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Memory>, AssistantError> {
        self.inner
            .lock()
            .map_err(|_| AssistantError::BackendUnavailable)
    }
}

#[async_trait]
impl FollowupStore for InMemoryStore {
    async fn insert_followup(&self, followup: Followup) -> Result<(), AssistantError> {
        let mut db = self.lock()?;
        if db
            .followups
            .iter()
            .any(|row| row.account_key == followup.account_key && row.id == followup.id)
        {
            return Err(AssistantError::InvalidArgument("duplicate followup".into()));
        }
        db.followups.push(followup);
        Ok(())
    }

    async fn get_followup(
        &self,
        account_key: &str,
        id: &str,
    ) -> Result<Option<Followup>, AssistantError> {
        let db = self.lock()?;
        Ok(db
            .followups
            .iter()
            .find(|row| row.account_key == account_key && row.id == id)
            .cloned())
    }

    async fn list_followups(
        &self,
        account_key: &str,
        filter: FollowupFilter,
    ) -> Result<Vec<Followup>, AssistantError> {
        let db = self.lock()?;
        let mut rows: Vec<_> = db
            .followups
            .iter()
            .filter(|row| row.account_key == account_key && matches_filter(row.status, filter))
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(rows)
    }

    async fn find_active(
        &self,
        account_key: &str,
        address: &str,
        thread_id: Option<&str>,
    ) -> Result<Option<Followup>, AssistantError> {
        let db = self.lock()?;
        Ok(db
            .followups
            .iter()
            .find(|row| {
                row.account_key == account_key
                    && row.status.is_active()
                    && row.counterpart_address.eq_ignore_ascii_case(address)
                    && row.thread_id.as_deref() == thread_id
            })
            .cloned())
    }

    async fn transition(
        &self,
        account_key: &str,
        id: &str,
        expect: FollowupStatus,
        followup: Followup,
        event: Option<AssistantEvent>,
    ) -> Result<bool, AssistantError> {
        let mut db = self.lock()?;
        let Some(row) = db
            .followups
            .iter_mut()
            .find(|row| row.account_key == account_key && row.id == id)
        else {
            return Err(AssistantError::NotFound);
        };
        if row.status != expect {
            return Ok(false);
        }
        *row = followup;
        if let Some(event) = event {
            db.events.push(event);
        }
        Ok(true)
    }

    async fn undelivered_events(
        &self,
        account_key: &str,
    ) -> Result<Vec<AssistantEvent>, AssistantError> {
        let db = self.lock()?;
        let mut rows: Vec<_> = db
            .events
            .iter()
            .filter(|event| event.account_key == account_key && event.delivered_at.is_none())
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(rows)
    }

    async fn mark_event_delivered(
        &self,
        account_key: &str,
        event_id: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AssistantError> {
        let mut db = self.lock()?;
        let Some(event) = db
            .events
            .iter_mut()
            .find(|event| event.account_key == account_key && event.id == event_id)
        else {
            return Err(AssistantError::NotFound);
        };
        if event.delivered_at.is_none() {
            event.delivered_at = Some(at);
        }
        Ok(())
    }

    async fn prefs(&self, account_key: &str) -> Result<Prefs, AssistantError> {
        let db = self.lock()?;
        Ok(db
            .prefs
            .iter()
            .find(|prefs| prefs.account_key == account_key)
            .cloned()
            .unwrap_or_else(|| Prefs::new(account_key)))
    }

    async fn put_prefs(&self, prefs: Prefs) -> Result<(), AssistantError> {
        let mut db = self.lock()?;
        if let Some(row) = db
            .prefs
            .iter_mut()
            .find(|row| row.account_key == prefs.account_key)
        {
            *row = prefs;
        } else {
            db.prefs.push(prefs);
        }
        Ok(())
    }

    async fn forget(&self, account_key: &str) -> Result<u64, AssistantError> {
        let mut db = self.lock()?;
        let before = db.followups.len();
        db.followups.retain(|row| row.account_key != account_key);
        let deleted = (before - db.followups.len()) as u64;
        db.events.retain(|event| event.account_key != account_key);
        db.prefs.retain(|prefs| prefs.account_key != account_key);
        Ok(deleted)
    }

    async fn open_followups(&self) -> Result<Vec<Followup>, AssistantError> {
        let db = self.lock()?;
        Ok(db
            .followups
            .iter()
            .filter(|row| row.status.is_active())
            .cloned()
            .collect())
    }

    async fn purge_closed_before(&self, cutoff: DateTime<Utc>) -> Result<u64, AssistantError> {
        let mut db = self.lock()?;
        let doomed: Vec<String> = db
            .followups
            .iter()
            .filter(|row| row.status.is_closed() && row.updated_at < cutoff)
            .map(|row| row.id.clone())
            .collect();
        let deleted = doomed.len() as u64;
        db.followups
            .retain(|row| !(row.status.is_closed() && row.updated_at < cutoff));
        db.events
            .retain(|event| !doomed.iter().any(|id| id == &event.followup_id));
        Ok(deleted)
    }
}

fn matches_filter(status: FollowupStatus, filter: FollowupFilter) -> bool {
    match filter {
        FollowupFilter::Active => status.is_active(),
        FollowupFilter::Replied => status == FollowupStatus::Replied,
        FollowupFilter::All => true,
    }
}

#[cfg(any(test, feature = "test-support"))]
pub async fn store_contract_suite(store: impl FollowupStore) {
    use crate::model::EventKind;

    let t0 = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 10, 1, 15, 0, 0)
        .single()
        .unwrap();
    let mut waiting = sample("acct-a", "fu-a", FollowupStatus::Waiting, t0);
    store.insert_followup(waiting.clone()).await.unwrap();
    let loaded = store
        .get_followup("acct-a", "fu-a")
        .await
        .unwrap()
        .expect("inserted row");
    assert_eq!(loaded, waiting);
    assert!(store
        .get_followup("acct-b", "fu-a")
        .await
        .unwrap()
        .is_none());

    let other = sample("acct-b", "fu-b", FollowupStatus::Waiting, t0);
    store.insert_followup(other.clone()).await.unwrap();
    let listed = store
        .list_followups("acct-a", FollowupFilter::Active)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(store
        .list_followups("acct-a", FollowupFilter::Replied)
        .await
        .unwrap()
        .is_empty());

    let found = store
        .find_active("acct-a", "Priya@Example.com", None)
        .await
        .unwrap();
    assert_eq!(found.unwrap().id, "fu-a");

    waiting.status = FollowupStatus::Overdue;
    waiting.updated_at = t0 + chrono::Duration::hours(1);
    let event = AssistantEvent {
        id: "ev-1".into(),
        account_key: "acct-a".into(),
        followup_id: "fu-a".into(),
        kind: EventKind::BecameOverdue,
        created_at: waiting.updated_at,
        delivered_at: None,
    };
    assert!(store
        .transition(
            "acct-a",
            "fu-a",
            FollowupStatus::Waiting,
            waiting.clone(),
            Some(event.clone()),
        )
        .await
        .unwrap());
    assert!(!store
        .transition(
            "acct-a",
            "fu-a",
            FollowupStatus::Waiting,
            waiting.clone(),
            Some(event.clone()),
        )
        .await
        .unwrap());
    assert_eq!(store.undelivered_events("acct-a").await.unwrap().len(), 1);
    assert!(store.undelivered_events("acct-b").await.unwrap().is_empty());
    store
        .mark_event_delivered("acct-a", "ev-1", waiting.updated_at)
        .await
        .unwrap();
    store
        .mark_event_delivered("acct-a", "ev-1", waiting.updated_at)
        .await
        .unwrap();
    assert!(store.undelivered_events("acct-a").await.unwrap().is_empty());

    let mut prefs = store.prefs("acct-a").await.unwrap();
    prefs.tz = Some("America/New_York".into());
    prefs.last_briefing_at = Some(t0);
    store.put_prefs(prefs.clone()).await.unwrap();
    assert_eq!(store.prefs("acct-a").await.unwrap(), prefs);

    let deleted = store.forget("acct-a").await.unwrap();
    assert_eq!(deleted, 1);
    assert!(store
        .list_followups("acct-a", FollowupFilter::All)
        .await
        .unwrap()
        .is_empty());
    assert!(store.undelivered_events("acct-a").await.unwrap().is_empty());
    assert_eq!(store.prefs("acct-a").await.unwrap(), Prefs::new("acct-a"));
    assert_eq!(
        store
            .list_followups("acct-b", FollowupFilter::Active)
            .await
            .unwrap()
            .len(),
        1
    );

    let mut closed = sample("acct-c", "fu-c", FollowupStatus::Replied, t0);
    closed.updated_at = t0;
    store.insert_followup(closed).await.unwrap();
    let purged = store
        .purge_closed_before(t0 + chrono::Duration::days(1))
        .await
        .unwrap();
    assert_eq!(purged, 1);
    assert!(store
        .get_followup("acct-c", "fu-c")
        .await
        .unwrap()
        .is_none());
    assert!(store
        .get_followup("acct-b", "fu-b")
        .await
        .unwrap()
        .is_some());
}

#[cfg(any(test, feature = "test-support"))]
fn sample(account: &str, id: &str, status: FollowupStatus, at: DateTime<Utc>) -> Followup {
    Followup {
        id: id.into(),
        account_key: account.into(),
        counterpart_address: "priya@example.com".into(),
        counterpart_name: Some("Priya Shah".into()),
        thread_id: None,
        about: Some("contract".into()),
        created_at: at,
        due_at: at + chrono::Duration::days(4),
        tz: "America/New_York".into(),
        baseline_at: at,
        status,
        snooze_until: None,
        replied_message_id: None,
        replied_at: None,
        draft_id: None,
        last_checked_at: None,
        updated_at: at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_store_matches_contract() {
        store_contract_suite(InMemoryStore::new()).await;
    }
}
