use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

use crate::errors::AssistantError;
use crate::model::{
    AssistantEvent, EventKind, Followup, FollowupFilter, FollowupStatus, DEFAULT_TZ, RETENTION_DAYS,
};
use crate::store::FollowupStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveAction {
    Done,
    Cancel,
    Snooze,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DueReminder {
    pub event: AssistantEvent,
    pub followup: Followup,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub became_overdue: Vec<Followup>,
    pub snooze_ended: Vec<Followup>,
    pub purged: u64,
}

pub fn parse_tz(name: &str) -> Result<Tz, AssistantError> {
    name.parse::<Tz>()
        .map_err(|_| AssistantError::InvalidArgument(format!("unknown timezone {name}")))
}

pub fn resolve_tz(requested: Option<&str>, saved: Option<&str>) -> Result<Tz, AssistantError> {
    parse_tz(requested.or(saved).unwrap_or(DEFAULT_TZ))
}

/// A date-only value is 18:00 in `tz`. A zoned timestamp is used as written.
/// `due <= now` is `invalid_argument`.
pub fn parse_due(raw: &str, tz: Tz, now: DateTime<Utc>) -> Result<DateTime<Utc>, AssistantError> {
    let raw = raw.trim();
    let due = if is_date_only(raw) {
        let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
            AssistantError::InvalidArgument("due must be an ISO date or datetime".into())
        })?;
        match tz.with_ymd_and_hms(date.year(), date.month(), date.day(), 18, 0, 0) {
            chrono::LocalResult::Single(local) => local.with_timezone(&Utc),
            chrono::LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&Utc),
            chrono::LocalResult::None => {
                return Err(AssistantError::InvalidArgument(
                    "due does not exist in that timezone".into(),
                ));
            }
        }
    } else if let Ok(zoned) = DateTime::parse_from_rfc3339(raw) {
        zoned.with_timezone(&Utc)
    } else {
        let naive =
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S").map_err(|_| {
                AssistantError::InvalidArgument("due must be an ISO date or datetime".into())
            })?;
        match tz.from_local_datetime(&naive) {
            chrono::LocalResult::Single(local) => local.with_timezone(&Utc),
            chrono::LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&Utc),
            chrono::LocalResult::None => {
                return Err(AssistantError::InvalidArgument(
                    "due does not exist in that timezone".into(),
                ));
            }
        }
    };
    if due <= now {
        return Err(AssistantError::InvalidArgument("due is in the past".into()));
    }
    Ok(due)
}

fn is_date_only(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() == 10
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && raw
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b'-')
}

pub async fn tick_account(
    store: &impl FollowupStore,
    account_key: &str,
    now: DateTime<Utc>,
) -> Result<TickReport, AssistantError> {
    let mut report = TickReport::default();
    let open = store
        .list_followups(account_key, FollowupFilter::Active)
        .await?;
    for followup in open {
        if let Some(updated) = end_snooze_if_due(store, &followup, now).await? {
            report.snooze_ended.push(updated.clone());
            if let Some(overdue) = mark_overdue_if_due(store, &updated, now).await? {
                report.became_overdue.push(overdue);
            }
            continue;
        }
        if let Some(overdue) = mark_overdue_if_due(store, &followup, now).await? {
            report.became_overdue.push(overdue);
        }
    }
    Ok(report)
}

/// Background pass over the memory table. Does not read a mailbox.
pub async fn tick_all(
    store: &impl FollowupStore,
    now: DateTime<Utc>,
) -> Result<TickReport, AssistantError> {
    let mut report = TickReport::default();
    let open = store.open_followups().await?;
    let mut accounts = Vec::new();
    for followup in &open {
        if !accounts
            .iter()
            .any(|key: &String| key == &followup.account_key)
        {
            accounts.push(followup.account_key.clone());
        }
    }
    for account in accounts {
        let part = tick_account(store, &account, now).await?;
        report.became_overdue.extend(part.became_overdue);
        report.snooze_ended.extend(part.snooze_ended);
    }
    report.purged = store
        .purge_closed_before(now - chrono::Duration::days(RETENTION_DAYS))
        .await?;
    Ok(report)
}

async fn end_snooze_if_due(
    store: &impl FollowupStore,
    followup: &Followup,
    now: DateTime<Utc>,
) -> Result<Option<Followup>, AssistantError> {
    if followup.status != FollowupStatus::Snoozed {
        return Ok(None);
    }
    let Some(until) = followup.snooze_until else {
        return Ok(None);
    };
    if now < until {
        return Ok(None);
    }
    let mut next = followup.clone();
    next.status = if now > next.due_at {
        FollowupStatus::Overdue
    } else {
        FollowupStatus::Waiting
    };
    next.snooze_until = None;
    next.updated_at = now;
    let event = Some(event_for(followup, EventKind::SnoozeEnded, now));
    if store
        .transition(
            &followup.account_key,
            &followup.id,
            FollowupStatus::Snoozed,
            next.clone(),
            event,
        )
        .await?
    {
        Ok(Some(next))
    } else {
        Ok(None)
    }
}

async fn mark_overdue_if_due(
    store: &impl FollowupStore,
    followup: &Followup,
    now: DateTime<Utc>,
) -> Result<Option<Followup>, AssistantError> {
    if followup.status != FollowupStatus::Waiting || now <= followup.due_at {
        return Ok(None);
    }
    let mut next = followup.clone();
    next.status = FollowupStatus::Overdue;
    next.updated_at = now;
    let event = Some(event_for(followup, EventKind::BecameOverdue, now));
    if store
        .transition(
            &followup.account_key,
            &followup.id,
            FollowupStatus::Waiting,
            next.clone(),
            event,
        )
        .await?
    {
        Ok(Some(next))
    } else {
        Ok(None)
    }
}

pub fn event_for(followup: &Followup, kind: EventKind, now: DateTime<Utc>) -> AssistantEvent {
    AssistantEvent {
        id: crate::model::new_id(),
        account_key: followup.account_key.clone(),
        followup_id: followup.id.clone(),
        kind,
        created_at: now,
        delivered_at: None,
    }
}

pub async fn resolve_followup(
    store: &impl FollowupStore,
    account_key: &str,
    id: &str,
    action: ResolveAction,
    until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Followup, AssistantError> {
    let Some(current) = store.get_followup(account_key, id).await? else {
        return Err(AssistantError::NotFound);
    };
    let mut next = current.clone();
    next.updated_at = now;
    match action {
        ResolveAction::Done => next.status = FollowupStatus::Done,
        ResolveAction::Cancel => next.status = FollowupStatus::Cancelled,
        ResolveAction::Snooze => {
            let Some(until) = until else {
                return Err(AssistantError::InvalidArgument(
                    "snooze requires until".into(),
                ));
            };
            if until <= now {
                return Err(AssistantError::InvalidArgument(
                    "snooze until is in the past".into(),
                ));
            }
            if !current.status.is_active() {
                return Err(AssistantError::InvalidArgument(
                    "only an open followup can be snoozed".into(),
                ));
            }
            next.status = FollowupStatus::Snoozed;
            next.snooze_until = Some(until);
        }
    }
    if current.status == next.status && action != ResolveAction::Snooze {
        return Ok(current);
    }
    let applied = store
        .transition(account_key, id, current.status, next.clone(), None)
        .await?;
    if !applied {
        return store
            .get_followup(account_key, id)
            .await?
            .ok_or(AssistantError::NotFound);
    }
    Ok(next)
}

pub async fn take_due_reminders(
    store: &impl FollowupStore,
    account_key: &str,
    now: DateTime<Utc>,
) -> Result<Vec<DueReminder>, AssistantError> {
    let events = store.undelivered_events(account_key).await?;
    let mut reminders = Vec::new();
    for event in events {
        store
            .mark_event_delivered(account_key, &event.id, now)
            .await?;
        let Some(followup) = store.get_followup(account_key, &event.followup_id).await? else {
            continue;
        };
        let mut delivered = event.clone();
        delivered.delivered_at = Some(now);
        reminders.push(DueReminder {
            event: delivered,
            followup,
        });
    }
    Ok(reminders)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryStore;

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).single().unwrap()
    }

    fn followup(status: FollowupStatus, due: DateTime<Utc>, now: DateTime<Utc>) -> Followup {
        Followup {
            id: "fu-1".into(),
            account_key: "acct".into(),
            counterpart_address: "priya@example.com".into(),
            counterpart_name: Some("Priya Shah".into()),
            thread_id: None,
            about: None,
            created_at: now,
            due_at: due,
            tz: "America/New_York".into(),
            baseline_at: now,
            status,
            snooze_until: None,
            replied_message_id: None,
            replied_at: None,
            draft_id: None,
            last_checked_at: None,
            updated_at: now,
        }
    }

    #[test]
    fn due_date_on_dst_fallback_is_eighteen_hundred_local() {
        let now = at(2026, 10, 1, 12);
        let tz = parse_tz("America/New_York").unwrap();
        let due = parse_due("2026-11-01", tz, now).unwrap();
        assert_eq!(due, at(2026, 11, 1, 23));
    }

    #[test]
    fn due_in_the_past_is_rejected() {
        let now = at(2026, 10, 19, 12);
        let tz = parse_tz("America/New_York").unwrap();
        let err = parse_due("2026-10-01", tz, now).unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
    }

    #[tokio::test]
    async fn snooze_ends_once() {
        let store = InMemoryStore::new();
        let now = at(2026, 10, 19, 12);
        let due = at(2026, 10, 23, 22);
        let mut row = followup(FollowupStatus::Waiting, due, now);
        store.insert_followup(row.clone()).await.unwrap();
        let snoozed = resolve_followup(
            &store,
            "acct",
            "fu-1",
            ResolveAction::Snooze,
            Some(now + chrono::Duration::hours(2)),
            now,
        )
        .await
        .unwrap();
        assert_eq!(snoozed.status, FollowupStatus::Snoozed);

        let early = tick_all(&store, now + chrono::Duration::hours(1))
            .await
            .unwrap();
        assert!(early.snooze_ended.is_empty());

        let ended = tick_all(&store, now + chrono::Duration::hours(3))
            .await
            .unwrap();
        assert_eq!(ended.snooze_ended.len(), 1);
        assert_eq!(ended.snooze_ended[0].status, FollowupStatus::Waiting);
        let again = tick_all(&store, now + chrono::Duration::hours(4))
            .await
            .unwrap();
        assert!(again.snooze_ended.is_empty());
        assert_eq!(store.undelivered_events("acct").await.unwrap().len(), 1);
        row.status = FollowupStatus::Waiting;
        let _ = row;
    }

    #[tokio::test]
    async fn reminder_is_marked_delivered_once() {
        let store = InMemoryStore::new();
        let now = at(2026, 10, 19, 12);
        let row = followup(FollowupStatus::Waiting, at(2026, 10, 19, 11), now);
        store.insert_followup(row).await.unwrap();
        tick_account(&store, "acct", now).await.unwrap();
        let first = take_due_reminders(&store, "acct", now).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].event.kind, EventKind::BecameOverdue);
        let second = take_due_reminders(&store, "acct", now).await.unwrap();
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn closed_followups_older_than_retention_are_purged() {
        let store = InMemoryStore::new();
        let now = at(2026, 10, 19, 12);
        let mut old = followup(
            FollowupStatus::Replied,
            now,
            now - chrono::Duration::days(40),
        );
        old.updated_at = now - chrono::Duration::days(31);
        old.id = "old".into();
        let mut fresh = followup(FollowupStatus::Done, now, now);
        fresh.id = "fresh".into();
        fresh.updated_at = now - chrono::Duration::days(1);
        let mut still_open = followup(
            FollowupStatus::Waiting,
            now + chrono::Duration::days(2),
            now,
        );
        still_open.id = "open".into();
        still_open.updated_at = now - chrono::Duration::days(40);
        store.insert_followup(old).await.unwrap();
        store.insert_followup(fresh).await.unwrap();
        store.insert_followup(still_open).await.unwrap();
        let report = tick_all(&store, now).await.unwrap();
        assert_eq!(report.purged, 1);
        assert!(store.get_followup("acct", "old").await.unwrap().is_none());
        assert!(store.get_followup("acct", "fresh").await.unwrap().is_some());
        assert!(store.get_followup("acct", "open").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn ticker_does_not_take_a_mailbox() {
        let store = InMemoryStore::new();
        let report = tick_all(&store, at(2026, 10, 19, 12)).await.unwrap();
        assert_eq!(report.purged, 0);
    }
}
