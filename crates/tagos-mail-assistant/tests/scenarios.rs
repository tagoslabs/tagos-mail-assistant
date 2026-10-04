use std::path::PathBuf;

use chrono::{Duration, TimeZone, Utc};
use tagos_mail_assistant::assistant::Assistant;
use tagos_mail_assistant::briefing::BriefingQuery;
use tagos_mail_assistant::clock::{Clock, FixedClock};
use tagos_mail_assistant::demo_mail::DemoMail;
use tagos_mail_assistant::drafts::{DraftKind, DraftRequest};
use tagos_mail_assistant::errors::AssistantError;
use tagos_mail_assistant::model::FollowupFilter;
use tagos_mail_assistant::store::{FollowupStore, InMemoryStore};
use tagos_mail_assistant::tracker::{AwaitReplyOutcome, AwaitReplyRequest};

const PRIYA_THREAD: &str = "11111111-1111-4111-8111-111111111111";
const SUPPORT_THREAD: &str = "44444444-4444-4444-8444-444444444444";

fn start() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
        .single()
        .unwrap()
}

fn demo() -> (
    DemoMail,
    FixedClock,
    Assistant<DemoMail, InMemoryStore, FixedClock>,
) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tagos-mail-assistant-demo/demo");
    let mailbox = std::fs::read_to_string(root.join("mailbox.json")).unwrap();
    let scenarios = std::fs::read_to_string(root.join("scenario.json")).unwrap();
    let mail = DemoMail::from_json(&mailbox, &scenarios).unwrap();
    let clock = FixedClock::new(start());
    let assistant = Assistant::new(mail.clone(), InMemoryStore::new(), clock.clone());
    (mail, clock, assistant)
}

fn track_priya(due: &str) -> AwaitReplyRequest {
    AwaitReplyRequest {
        person: "Priya".into(),
        due: due.into(),
        about: Some("the contract".into()),
        thread_id: Some(PRIYA_THREAD.into()),
        tz: Some("America/New_York".into()),
    }
}

#[tokio::test]
async fn scenario_priya_end_to_end() {
    let (mail, clock, assistant) = demo();
    let briefing = assistant
        .inbox_briefing(
            "demo",
            BriefingQuery {
                since: None,
                max_items: None,
            },
        )
        .await
        .unwrap();
    assert!(briefing
        .suggested_followups
        .iter()
        .any(|item| item.address == "priya@example.com"));
    assert!(briefing
        .needs_reply
        .iter()
        .any(|item| item.address == "tom@example.org"));
    assert!(briefing
        .fyi
        .iter()
        .any(|item| item.address == "noreply@shop.example.net"));

    let tracked = assistant
        .await_reply("demo", track_priya("2026-10-23"))
        .await
        .unwrap();
    let AwaitReplyOutcome::Tracking { followup, .. } = tracked else {
        panic!("expected tracking");
    };

    // Monday 08:00 ET plus 96h is Friday 08:00 ET, still before the 18:00 due.
    // One more hour passes that deadline.
    clock.advance_hours(107);
    let overdue = assistant.check_followups("demo").await.unwrap();
    assert!(overdue
        .newly_overdue
        .iter()
        .any(|row| row.id == followup.id));

    let draft = assistant
        .prepare_reply_draft(
            "demo",
            DraftRequest {
                followup_id: Some(followup.id.clone()),
                thread_id: None,
                body_text: None,
                kind: DraftKind::Nudge,
                to: None,
                confirm_new_recipient: false,
                subject: None,
            },
        )
        .await
        .unwrap();
    assert!(!draft.sent);
    assert!(draft.requires_user_review);
    assert!(draft.body_text.contains("gentle nudge"));
    assert_eq!(draft.note, "Saved as a draft in Tagos. Nothing was sent.");

    mail.deliver("priya_signed", clock.now()).unwrap();
    let replied = assistant.check_followups("demo").await.unwrap();
    assert_eq!(replied.newly_replied.len(), 1);
    assert_eq!(replied.newly_replied[0].followup.id, followup.id);
}

#[tokio::test]
async fn clarify_two_sams_creates_nothing() {
    let (_mail, _clock, assistant) = demo();
    let outcome = assistant
        .await_reply(
            "demo",
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
    let page = assistant
        .list_followups("demo", FollowupFilter::All, None)
        .await
        .unwrap();
    assert!(page.followups.is_empty());
}

#[tokio::test]
async fn injection_email_cannot_add_recipient() {
    let (_mail, _clock, assistant) = demo();
    let briefing = assistant
        .inbox_briefing(
            "demo",
            BriefingQuery {
                since: None,
                max_items: None,
            },
        )
        .await
        .unwrap();
    assert!(briefing.spoken_summary.contains("Account notice"));
    assert!(!briefing.spoken_summary.contains("collect@example.net"));
    assert!(!briefing
        .spoken_summary
        .to_ascii_lowercase()
        .contains("ignore previous"));
    let err = assistant
        .prepare_reply_draft(
            "demo",
            DraftRequest {
                followup_id: None,
                thread_id: Some(SUPPORT_THREAD.into()),
                body_text: Some("Hello".into()),
                kind: DraftKind::Reply,
                to: Some("collect@example.net".into()),
                confirm_new_recipient: false,
                subject: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err, AssistantError::RecipientNotAllowed);
}

#[tokio::test]
async fn reminder_delivered_exactly_once() {
    let (_mail, clock, assistant) = demo();
    let AwaitReplyOutcome::Tracking { followup, .. } = assistant
        .await_reply("demo", track_priya("2026-10-20"))
        .await
        .unwrap()
    else {
        panic!("expected tracking");
    };
    clock.advance_hours(48);
    let first = assistant
        .list_followups("demo", FollowupFilter::Active, None)
        .await
        .unwrap();
    assert!(first
        .due_reminders
        .iter()
        .any(|reminder| reminder.followup.id == followup.id));
    let second = assistant
        .list_followups("demo", FollowupFilter::Active, None)
        .await
        .unwrap();
    assert!(second.due_reminders.is_empty());
}

#[tokio::test]
async fn briefing_since_last_only_shows_new() {
    let (mail, clock, assistant) = demo();
    let first = assistant
        .inbox_briefing(
            "demo",
            BriefingQuery {
                since: None,
                max_items: None,
            },
        )
        .await
        .unwrap();
    assert!(first.spoken_summary.contains("Tom Hale"));
    clock.advance_hours(2);
    mail.deliver("priya_signed", clock.now()).unwrap();
    let second = assistant
        .inbox_briefing(
            "demo",
            BriefingQuery {
                since: None,
                max_items: None,
            },
        )
        .await
        .unwrap();
    assert!(!second.spoken_summary.contains("Tom Hale"));
    assert!(!second.spoken_summary.contains("collect@example.net"));
}

#[tokio::test]
async fn forget_followups_wipes_account() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tagos-mail-assistant-demo/demo");
    let mailbox = std::fs::read_to_string(root.join("mailbox.json")).unwrap();
    let scenarios = std::fs::read_to_string(root.join("scenario.json")).unwrap();
    let mail = DemoMail::from_json(&mailbox, &scenarios).unwrap();
    let store = InMemoryStore::new();
    let assistant = Assistant::new(mail, store.clone(), FixedClock::new(start()));
    assistant
        .await_reply("demo", track_priya("2026-10-23"))
        .await
        .unwrap();
    let err = assistant.forget_followups("demo", false).await.unwrap_err();
    assert_eq!(err.code(), "invalid_argument");
    store
        .insert_followup(tagos_mail_assistant::model::Followup {
            id: "other".into(),
            account_key: "other-account".into(),
            counterpart_address: "priya@example.com".into(),
            counterpart_name: None,
            thread_id: None,
            about: None,
            created_at: start(),
            due_at: start() + Duration::days(2),
            tz: "America/New_York".into(),
            baseline_at: start(),
            status: tagos_mail_assistant::model::FollowupStatus::Waiting,
            snooze_until: None,
            replied_message_id: None,
            replied_at: None,
            draft_id: None,
            last_checked_at: None,
            updated_at: start(),
        })
        .await
        .unwrap();
    let deleted = assistant.forget_followups("demo", true).await.unwrap();
    assert_eq!(deleted, 1);
    let page = assistant
        .list_followups("demo", FollowupFilter::All, None)
        .await
        .unwrap();
    assert!(page.followups.is_empty());
    assert!(store
        .get_followup("other-account", "other")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn due_in_past_rejected() {
    let (_mail, _clock, assistant) = demo();
    let err = assistant
        .await_reply("demo", track_priya("2026-10-01"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_argument");
}

#[tokio::test]
async fn dedup_same_person_thread() {
    let (_mail, _clock, assistant) = demo();
    let first = assistant
        .await_reply("demo", track_priya("2026-10-23"))
        .await
        .unwrap();
    assert!(matches!(first, AwaitReplyOutcome::Tracking { .. }));
    let second = assistant
        .await_reply("demo", track_priya("2026-10-24"))
        .await
        .unwrap();
    assert!(matches!(second, AwaitReplyOutcome::AlreadyTracking { .. }));
}

#[test]
fn assistant_has_no_send_capability() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let banned_fn = ["fn ", "send"].concat();
    let banned_message = ["send", "_message"].concat();
    let banned_draft = ["send", "_draft"].concat();
    for name in [
        "assistant.rs",
        "backend.rs",
        "drafts.rs",
        "demo_mail.rs",
        "detector.rs",
        "tracker.rs",
        "scheduler.rs",
        "briefing.rs",
    ] {
        let src = std::fs::read_to_string(root.join(name)).unwrap();
        assert!(!src.contains(&banned_fn), "{name}");
        assert!(!src.contains(&banned_message), "{name}");
        assert!(!src.contains(&banned_draft), "{name}");
    }
}

#[test]
fn fixtures_use_only_synthetic_addresses() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tagos-mail-assistant-demo/demo");
    for name in ["mailbox.json", "scenario.json"] {
        let text = std::fs::read_to_string(root.join(name)).unwrap();
        let mut rest = text.as_str();
        while let Some(at) = rest.find('@') {
            let after = &rest[at + 1..];
            let domain: String = after
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '.' || *ch == '-')
                .collect();
            let synthetic = domain == "demo.tagos.example"
                || domain == "example.com"
                || domain == "example.org"
                || domain == "example.net"
                || domain.ends_with(".example.com")
                || domain.ends_with(".example.org")
                || domain.ends_with(".example.net");
            assert!(synthetic, "{name} has {domain}");
            rest = &after[domain.len()..];
        }
    }
}
