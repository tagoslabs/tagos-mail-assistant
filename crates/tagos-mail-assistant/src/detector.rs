use chrono::{DateTime, Utc};

use crate::backend::MailBackend;
use crate::clock::Clock;
use crate::errors::AssistantError;
use crate::model::{
    EventKind, Followup, FollowupFilter, FollowupStatus, MessageView, DETECTOR_CALL_BUDGET,
    READ_LIMIT_MAX,
};
use crate::scheduler::{event_for, tick_account};
use crate::store::FollowupStore;
use crate::untrusted::{same_addr, sanitize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyNotice {
    pub followup: Followup,
    pub message_id: String,
    pub from: String,
    pub received_at: DateTime<Utc>,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub newly_replied: Vec<ReplyNotice>,
    pub newly_overdue: Vec<Followup>,
    pub still_waiting: Vec<Followup>,
    pub deferred: bool,
}

pub async fn check_followups(
    backend: &impl MailBackend,
    store: &impl FollowupStore,
    clock: &impl Clock,
    account_key: &str,
) -> Result<CheckReport, AssistantError> {
    let now = clock.now();
    let ticked = tick_account(store, account_key, now).await?;
    let mut report = CheckReport {
        newly_replied: Vec::new(),
        newly_overdue: ticked.became_overdue,
        still_waiting: Vec::new(),
        deferred: false,
    };
    let mut budget = DETECTOR_CALL_BUDGET;
    let open = store
        .list_followups(account_key, FollowupFilter::Active)
        .await?;
    for followup in open {
        if followup.status == FollowupStatus::Replied {
            continue;
        }
        match find_reply(backend, &followup, &mut budget).await {
            Ok(Some(message)) => {
                if let Some(notice) = mark_replied(store, &followup, &message, now).await? {
                    report.newly_overdue.retain(|row| row.id != followup.id);
                    report.newly_replied.push(notice);
                }
            }
            Ok(None) => {
                if followup.status == FollowupStatus::Waiting
                    && !report.newly_overdue.iter().any(|row| row.id == followup.id)
                {
                    report.still_waiting.push(followup);
                }
            }
            Err(AssistantError::RateLimited) => {
                report.deferred = true;
                break;
            }
            Err(error) => return Err(error),
        }
    }
    if !report.deferred {
        let mut prefs = store.prefs(account_key).await?;
        prefs.last_mail_check_at = Some(now);
        store.put_prefs(prefs).await?;
    }
    Ok(report)
}

async fn find_reply(
    backend: &impl MailBackend,
    followup: &Followup,
    budget: &mut u32,
) -> Result<Option<MessageView>, AssistantError> {
    if let Some(thread_id) = &followup.thread_id {
        let messages = spend(backend.get_thread(thread_id, READ_LIMIT_MAX), budget).await?;
        return Ok(first_reply(followup, &messages));
    }
    let contexts = spend(
        backend.list_contexts(&followup.counterpart_address, 5),
        budget,
    )
    .await?;
    let mut candidates: Vec<_> = contexts
        .into_iter()
        .filter(|thread| thread.last_message_at > followup.baseline_at)
        .collect();
    candidates.sort_by_key(|thread| std::cmp::Reverse(thread.last_message_at));
    candidates.truncate(3);
    for thread in candidates {
        let messages = spend(
            backend.get_thread(&thread.thread_id, READ_LIMIT_MAX),
            budget,
        )
        .await?;
        if let Some(message) = first_reply(followup, &messages) {
            return Ok(Some(message));
        }
    }
    Ok(None)
}

async fn spend<T>(
    call: impl std::future::Future<Output = Result<T, AssistantError>>,
    budget: &mut u32,
) -> Result<T, AssistantError> {
    if *budget == 0 {
        return Err(AssistantError::RateLimited);
    }
    *budget -= 1;
    call.await
}

fn first_reply(followup: &Followup, messages: &[MessageView]) -> Option<MessageView> {
    messages
        .iter()
        .find(|message| is_reply(followup, message))
        .cloned()
}

fn is_reply(followup: &Followup, message: &MessageView) -> bool {
    message.received_at > followup.baseline_at
        && same_addr(&message.from, &followup.counterpart_address)
}

async fn mark_replied(
    store: &impl FollowupStore,
    followup: &Followup,
    message: &MessageView,
    now: DateTime<Utc>,
) -> Result<Option<ReplyNotice>, AssistantError> {
    let mut next = followup.clone();
    next.status = FollowupStatus::Replied;
    next.replied_message_id = Some(message.message_id.clone());
    next.replied_at = Some(message.received_at);
    next.updated_at = now;
    next.last_checked_at = Some(now);
    let event = event_for(followup, EventKind::ReplyArrived, now);
    let applied = store
        .transition(
            &followup.account_key,
            &followup.id,
            followup.status,
            next.clone(),
            Some(event),
        )
        .await?;
    if !applied {
        return Ok(None);
    }
    Ok(Some(ReplyNotice {
        followup: next,
        message_id: message.message_id.clone(),
        from: message.from.clone(),
        received_at: message.received_at,
        snippet: sanitize(&message.body_text),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::model::{MessageView, Person, ThreadView};
    use crate::store::InMemoryStore;
    use crate::testing::FakeMail;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
            .single()
            .unwrap()
    }

    fn waiting(thread: Option<&str>, baseline: DateTime<Utc>) -> Followup {
        Followup {
            id: "fu-1".into(),
            account_key: "acct".into(),
            counterpart_address: "priya@example.com".into(),
            counterpart_name: Some("Priya Shah".into()),
            thread_id: thread.map(str::to_string),
            about: None,
            created_at: baseline,
            due_at: baseline + chrono::Duration::days(4),
            tz: "America/New_York".into(),
            baseline_at: baseline,
            status: FollowupStatus::Waiting,
            snooze_until: None,
            replied_message_id: None,
            replied_at: None,
            draft_id: None,
            last_checked_at: None,
            updated_at: baseline,
        }
    }

    #[tokio::test]
    async fn reply_after_baseline_closes_followup_once() {
        let baseline = now() - chrono::Duration::days(3);
        let mut mail = FakeMail::new();
        mail.messages = vec![
            MessageView {
                message_id: "m-out".into(),
                thread_id: "thread-priya".into(),
                from: "Alex <alex@demo.tagos.example>".into(),
                to: "priya@example.com".into(),
                subject: "Contract for signature".into(),
                body_text: "Please sign.".into(),
                received_at: baseline,
                folder: "sent".into(),
            },
            MessageView {
                message_id: "m-in".into(),
                thread_id: "thread-priya".into(),
                from: "Priya Shah <Priya@Example.com>".into(),
                to: "alex@demo.tagos.example".into(),
                subject: "Re: Contract for signature".into(),
                body_text: "Signed.".into(),
                received_at: now() - chrono::Duration::hours(1),
                folder: "inbox".into(),
            },
        ];
        let store = InMemoryStore::new();
        let mut row = waiting(Some("thread-priya"), baseline);
        row.due_at = now() + chrono::Duration::days(2);
        store.insert_followup(row).await.unwrap();
        let clock = FixedClock::new(now());
        let first = check_followups(&mail, &store, &clock, "acct")
            .await
            .unwrap();
        assert_eq!(first.newly_replied.len(), 1);
        assert_eq!(first.newly_replied[0].message_id, "m-in");
        assert!(first.newly_replied[0].snippet.contains("Signed"));
        let second = check_followups(&mail, &store, &clock, "acct")
            .await
            .unwrap();
        assert!(second.newly_replied.is_empty());
        let events = store.undelivered_events("acct").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::ReplyArrived);
    }

    #[tokio::test]
    async fn rate_limit_stops_without_retry() {
        let baseline = now() - chrono::Duration::hours(2);
        let mut mail = FakeMail::new();
        mail.fail_after = Some(1);
        mail.people = vec![Person {
            address: "priya@example.com".into(),
            name: None,
            last_activity: Some(now()),
            unread_count: 0,
        }];
        mail.contexts = vec![
            (
                "priya@example.com".into(),
                ThreadView {
                    thread_id: "t1".into(),
                    subject: "One".into(),
                    last_message_at: now(),
                    unread_count: 0,
                },
            ),
            (
                "a@example.com".into(),
                ThreadView {
                    thread_id: "t2".into(),
                    subject: "Two".into(),
                    last_message_at: now(),
                    unread_count: 0,
                },
            ),
        ];
        let store = InMemoryStore::new();
        let mut first = waiting(None, baseline);
        first.due_at = now() + chrono::Duration::days(1);
        let mut second = first.clone();
        second.id = "fu-2".into();
        second.counterpart_address = "a@example.com".into();
        store.insert_followup(first).await.unwrap();
        store.insert_followup(second).await.unwrap();
        let clock = FixedClock::new(now());
        let report = check_followups(&mail, &store, &clock, "acct")
            .await
            .unwrap();
        assert!(report.deferred);
        let calls = mail.calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(calls <= DETECTOR_CALL_BUDGET + 1);
        let again = check_followups(&mail, &store, &clock, "acct")
            .await
            .unwrap();
        assert!(again.deferred);
    }
}
