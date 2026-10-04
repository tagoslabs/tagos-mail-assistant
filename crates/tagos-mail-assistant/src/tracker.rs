use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::backend::MailBackend;
use crate::clock::Clock;
use crate::detector::check_followups;
use crate::errors::AssistantError;
use crate::model::{
    Candidate, Followup, FollowupStatus, Person, ThreadView, ABOUT_MAX_CHARS, LAZY_CHECK_SECS,
    SUGGESTED_MAX_AGE_DAYS,
};
use crate::scheduler::{parse_due, resolve_tz};
use crate::store::FollowupStore;
use crate::untrusted::{addr_spec, sanitize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitReplyRequest {
    pub person: String,
    pub due: String,
    pub about: Option<String>,
    pub thread_id: Option<String>,
    pub tz: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwaitReplyOutcome {
    Tracking {
        followup: Followup,
        confirmation_text: String,
        suggested_thread: Option<ThreadView>,
    },
    NeedsClarification {
        candidates: Vec<Candidate>,
    },
    AlreadyTracking {
        followup: Followup,
    },
}

pub async fn await_reply(
    backend: &impl MailBackend,
    store: &impl FollowupStore,
    clock: &impl Clock,
    account_key: &str,
    request: AwaitReplyRequest,
) -> Result<AwaitReplyOutcome, AssistantError> {
    if request.person.trim().is_empty() {
        return Err(AssistantError::InvalidArgument("person is required".into()));
    }
    let about = normalize_about(request.about)?;
    lazy_check(backend, store, clock, account_key).await?;

    let people = backend.list_people(Some(request.person.trim()), 20).await?;
    let resolved = match people.as_slice() {
        [] => match explicit_address(&request.person) {
            Some(address) => Resolved {
                address,
                name: display_name(&request.person),
            },
            None => {
                return Ok(AwaitReplyOutcome::NeedsClarification {
                    candidates: Vec::new(),
                });
            }
        },
        [one] => Resolved::from_person(one),
        many => {
            return Ok(AwaitReplyOutcome::NeedsClarification {
                candidates: many.iter().map(Candidate::from).collect(),
            });
        }
    };

    let prefs = store.prefs(account_key).await?;
    let tz = resolve_tz(request.tz.as_deref(), prefs.tz.as_deref())?;
    let now = clock.now();
    let due_at = parse_due(&request.due, tz, now)?;
    let thread_id = request.thread_id.clone();
    if let Some(existing) = store
        .find_active(account_key, &resolved.address, thread_id.as_deref())
        .await?
    {
        return Ok(AwaitReplyOutcome::AlreadyTracking { followup: existing });
    }

    let suggested_thread = if thread_id.is_none() {
        suggest_thread(backend, &resolved.address, now).await?
    } else {
        None
    };
    let followup = Followup {
        id: crate::model::new_id(),
        account_key: account_key.to_string(),
        counterpart_address: resolved.address.clone(),
        counterpart_name: resolved.name.clone(),
        thread_id,
        about,
        created_at: now,
        due_at,
        tz: tz.name().to_string(),
        baseline_at: now,
        status: FollowupStatus::Waiting,
        snooze_until: None,
        replied_message_id: None,
        replied_at: None,
        draft_id: None,
        last_checked_at: Some(now),
        updated_at: now,
    };
    store.insert_followup(followup.clone()).await?;
    let confirmation_text = confirmation(&followup, tz);
    Ok(AwaitReplyOutcome::Tracking {
        followup,
        confirmation_text,
        suggested_thread,
    })
}

struct Resolved {
    address: String,
    name: Option<String>,
}

impl Resolved {
    fn from_person(person: &Person) -> Self {
        Self {
            address: addr_spec(&person.address),
            name: person.name.clone(),
        }
    }
}

impl From<&Person> for Candidate {
    fn from(person: &Person) -> Self {
        Self {
            address: person.address.clone(),
            name: person.name.clone(),
            last_activity: person.last_activity,
        }
    }
}

pub async fn lazy_check(
    backend: &impl MailBackend,
    store: &impl FollowupStore,
    clock: &impl Clock,
    account_key: &str,
) -> Result<(), AssistantError> {
    let prefs = store.prefs(account_key).await?;
    let now = clock.now();
    let stale = match prefs.last_mail_check_at {
        None => true,
        Some(at) => (now - at).num_seconds() >= LAZY_CHECK_SECS,
    };
    if stale {
        check_followups(backend, store, clock, account_key).await?;
    }
    Ok(())
}

fn normalize_about(about: Option<String>) -> Result<Option<String>, AssistantError> {
    let Some(about) = about else {
        return Ok(None);
    };
    let trimmed = about.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > ABOUT_MAX_CHARS {
        return Err(AssistantError::InvalidArgument("about is too long".into()));
    }
    Ok(Some(trimmed.to_string()))
}

fn explicit_address(person: &str) -> Option<String> {
    let spec = addr_spec(person);
    if spec.contains('@') && spec.contains('.') {
        Some(spec)
    } else {
        None
    }
}

fn display_name(person: &str) -> Option<String> {
    let open = person.find('<')?;
    let name = person[..open].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

async fn suggest_thread(
    backend: &impl MailBackend,
    address: &str,
    now: DateTime<Utc>,
) -> Result<Option<ThreadView>, AssistantError> {
    let contexts = backend.list_contexts(address, 5).await?;
    let cutoff = now - chrono::Duration::days(SUGGESTED_MAX_AGE_DAYS);
    Ok(contexts
        .into_iter()
        .filter(|thread| thread.last_message_at >= cutoff)
        .max_by_key(|thread| thread.last_message_at))
}

fn confirmation(followup: &Followup, tz: Tz) -> String {
    let who = followup
        .counterpart_name
        .as_deref()
        .map(sanitize)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| followup.counterpart_address.clone());
    let local = followup.due_at.with_timezone(&tz).format("%Y-%m-%d %H:%M");
    let tz_name = tz.name();
    format!("I'll watch for a reply from {who} until {local} ({tz_name}).")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::model::ThreadView;
    use crate::store::InMemoryStore;
    use crate::testing::FakeMail;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
            .single()
            .unwrap()
    }

    fn person(address: &str, name: &str) -> Person {
        Person {
            address: address.into(),
            name: Some(name.into()),
            last_activity: Some(now()),
            unread_count: 0,
        }
    }

    #[tokio::test]
    async fn two_matches_create_nothing() {
        let mut mail = FakeMail::new();
        mail.people = vec![
            person("sam.lee@example.com", "Sam Lee"),
            person("sam.ortiz@example.org", "Sam Ortiz"),
        ];
        let store = InMemoryStore::new();
        let clock = FixedClock::new(now());
        let outcome = await_reply(
            &mail,
            &store,
            &clock,
            "acct",
            AwaitReplyRequest {
                person: "Sam".into(),
                due: "2026-10-23".into(),
                about: None,
                thread_id: None,
                tz: Some("America/New_York".into()),
            },
        )
        .await
        .unwrap();
        match outcome {
            AwaitReplyOutcome::NeedsClarification { candidates } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected clarification, got {other:?}"),
        }
        assert!(store
            .list_followups("acct", crate::model::FollowupFilter::All)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn same_person_and_thread_is_deduped() {
        let mut mail = FakeMail::new();
        mail.people = vec![person("priya@example.com", "Priya Shah")];
        let store = InMemoryStore::new();
        let clock = FixedClock::new(now());
        let request = AwaitReplyRequest {
            person: "Priya".into(),
            due: "2026-10-23".into(),
            about: Some("contract".into()),
            thread_id: Some("thread-priya".into()),
            tz: Some("America/New_York".into()),
        };
        let first = await_reply(&mail, &store, &clock, "acct", request.clone())
            .await
            .unwrap();
        assert!(matches!(first, AwaitReplyOutcome::Tracking { .. }));
        let second = await_reply(&mail, &store, &clock, "acct", request)
            .await
            .unwrap();
        assert!(matches!(second, AwaitReplyOutcome::AlreadyTracking { .. }));
        assert_eq!(
            store
                .list_followups("acct", crate::model::FollowupFilter::All)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn explicit_address_with_no_match_is_tracked() {
        let mail = FakeMail::new();
        let store = InMemoryStore::new();
        let clock = FixedClock::new(now());
        let outcome = await_reply(
            &mail,
            &store,
            &clock,
            "acct",
            AwaitReplyRequest {
                person: "Priya Shah <priya@example.com>".into(),
                due: "2026-10-23T22:00:00Z".into(),
                about: None,
                thread_id: None,
                tz: None,
            },
        )
        .await
        .unwrap();
        match outcome {
            AwaitReplyOutcome::Tracking { followup, .. } => {
                assert_eq!(followup.counterpart_address, "priya@example.com");
                assert_eq!(followup.counterpart_name.as_deref(), Some("Priya Shah"));
            }
            other => panic!("expected tracking, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_name_creates_nothing() {
        let mail = FakeMail::new();
        let store = InMemoryStore::new();
        let clock = FixedClock::new(now());
        let outcome = await_reply(
            &mail,
            &store,
            &clock,
            "acct",
            AwaitReplyRequest {
                person: "Nobody".into(),
                due: "2026-10-23".into(),
                about: None,
                thread_id: None,
                tz: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            AwaitReplyOutcome::NeedsClarification { candidates } if candidates.is_empty()
        ));
    }

    #[tokio::test]
    async fn suggests_a_recent_thread_without_attaching_it() {
        let mut mail = FakeMail::new();
        mail.people = vec![person("priya@example.com", "Priya Shah")];
        mail.contexts = vec![(
            "priya@example.com".into(),
            ThreadView {
                thread_id: "thread-priya".into(),
                subject: "Contract for signature".into(),
                last_message_at: now() - chrono::Duration::days(3),
                unread_count: 0,
            },
        )];
        let store = InMemoryStore::new();
        let clock = FixedClock::new(now());
        let outcome = await_reply(
            &mail,
            &store,
            &clock,
            "acct",
            AwaitReplyRequest {
                person: "priya@example.com".into(),
                due: "2026-10-23".into(),
                about: None,
                thread_id: None,
                tz: Some("America/New_York".into()),
            },
        )
        .await
        .unwrap();
        match outcome {
            AwaitReplyOutcome::Tracking {
                followup,
                suggested_thread,
                ..
            } => {
                assert!(followup.thread_id.is_none());
                assert_eq!(suggested_thread.unwrap().thread_id, "thread-priya");
            }
            other => panic!("expected tracking, got {other:?}"),
        }
    }
}
