use crate::backend::MailBackend;
use crate::errors::AssistantError;
use crate::guards::{quote_exceeds_guard, recipient_decision};
use crate::model::{
    DraftRecord, Followup, MessageView, NewDraft, BODY_TEXT_MAX_CHARS, READ_LIMIT_MAX,
};
use crate::store::FollowupStore;
use crate::untrusted::{addr_spec, same_addr};

pub const DRAFT_NOTE: &str = "Saved as a draft in Tagos. Nothing was sent.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftKind {
    Nudge,
    Reply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRequest {
    pub followup_id: Option<String>,
    pub thread_id: Option<String>,
    pub body_text: Option<String>,
    pub kind: DraftKind,
    pub to: Option<String>,
    pub confirm_new_recipient: bool,
    pub subject: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDraft {
    pub draft_id: String,
    pub to: String,
    pub subject: String,
    pub body_text: String,
    pub thread_id: Option<String>,
    pub sent: bool,
    pub requires_user_review: bool,
    pub new_recipient: bool,
    pub note: String,
}

pub fn reply_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("re:") {
        trimmed.to_string()
    } else if trimmed.is_empty() {
        "Re:".to_string()
    } else {
        format!("Re: {trimmed}")
    }
}

pub fn nudge_body(name: &str, topic: &str) -> String {
    format!(
        "Hi {name},\n\nJust a gentle nudge about {topic}. Happy to help if anything is blocking this.\n"
    )
}

pub fn reply_body(name: &str, topic: &str) -> String {
    format!("Hi {name},\n\nThanks for your note about {topic}.\n")
}

pub async fn prepare_reply_draft(
    backend: &impl MailBackend,
    store: &impl FollowupStore,
    account_key: &str,
    request: DraftRequest,
) -> Result<PreparedDraft, AssistantError> {
    let (followup, thread_id) = resolve_target(store, account_key, &request).await?;
    let messages = if let Some(thread_id) = &thread_id {
        backend.get_thread(thread_id, READ_LIMIT_MAX).await?
    } else {
        Vec::new()
    };
    let allowed = allowed_addresses(backend.owner_address(), followup.as_ref(), &messages);
    let (to, new_recipient) = choose_recipient(&request, &allowed, followup.as_ref())?;
    let subject = choose_subject(&request, &messages);
    let body_text = choose_body(&request, followup.as_ref(), &messages, &to)?;
    let sources: Vec<&str> = messages
        .iter()
        .map(|message| message.body_text.as_str())
        .collect();
    if quote_exceeds_guard(&body_text, &sources) {
        return Err(AssistantError::InvalidArgument(
            "draft quotes too much of an email".into(),
        ));
    }
    if body_text.chars().count() > BODY_TEXT_MAX_CHARS {
        return Err(AssistantError::InvalidArgument("body is too long".into()));
    }
    let record = backend
        .create_draft(NewDraft {
            to: to.clone(),
            subject: subject.clone(),
            body_text: body_text.clone(),
            thread_id: thread_id.clone(),
        })
        .await?;
    if let Some(followup) = followup {
        remember_draft(store, &followup, &record).await?;
    }
    Ok(PreparedDraft {
        draft_id: record.draft_id,
        to,
        subject,
        body_text,
        thread_id,
        sent: false,
        requires_user_review: true,
        new_recipient,
        note: DRAFT_NOTE.to_string(),
    })
}

async fn resolve_target(
    store: &impl FollowupStore,
    account_key: &str,
    request: &DraftRequest,
) -> Result<(Option<Followup>, Option<String>), AssistantError> {
    match (&request.followup_id, &request.thread_id) {
        (Some(_), Some(_)) | (None, None) => Err(AssistantError::InvalidArgument(
            "pass exactly one of followup_id or thread_id".into(),
        )),
        (Some(id), None) => {
            let followup = store
                .get_followup(account_key, id)
                .await?
                .ok_or(AssistantError::NotFound)?;
            let thread_id = followup.thread_id.clone();
            Ok((Some(followup), thread_id))
        }
        (None, Some(thread_id)) => Ok((None, Some(thread_id.clone()))),
    }
}

fn allowed_addresses(
    owner: &str,
    followup: Option<&Followup>,
    messages: &[MessageView],
) -> Vec<String> {
    let mut allowed = Vec::new();
    if let Some(followup) = followup {
        push_addr(&mut allowed, owner, &followup.counterpart_address);
    }
    for message in messages {
        push_addr(&mut allowed, owner, &message.from);
        push_addr(&mut allowed, owner, &message.to);
    }
    allowed
}

fn push_addr(allowed: &mut Vec<String>, owner: &str, raw: &str) {
    let spec = addr_spec(raw);
    if !spec.contains('@') || same_addr(&spec, owner) {
        return;
    }
    if allowed.iter().any(|item| same_addr(item, &spec)) {
        return;
    }
    allowed.push(spec);
}

fn choose_recipient(
    request: &DraftRequest,
    allowed: &[String],
    followup: Option<&Followup>,
) -> Result<(String, bool), AssistantError> {
    if let Some(to) = &request.to {
        let new_recipient = recipient_decision(to, allowed, request.confirm_new_recipient)?;
        return Ok((addr_spec(to), new_recipient));
    }
    if let Some(followup) = followup {
        return Ok((followup.counterpart_address.clone(), false));
    }
    let Some(default) = allowed.first() else {
        return Err(AssistantError::InvalidArgument(
            "could not tell who should receive this draft".into(),
        ));
    };
    Ok((default.clone(), false))
}

fn choose_subject(request: &DraftRequest, messages: &[MessageView]) -> String {
    if let Some(subject) = request.subject.as_deref() {
        return subject.trim().to_string();
    }
    let existing = messages
        .last()
        .map(|message| message.subject.as_str())
        .unwrap_or("");
    reply_subject(existing)
}

fn choose_body(
    request: &DraftRequest,
    followup: Option<&Followup>,
    messages: &[MessageView],
    to: &str,
) -> Result<String, AssistantError> {
    if let Some(body) = request.body_text.as_deref() {
        return Ok(body.to_string());
    }
    let name = followup
        .and_then(|row| row.counterpart_name.clone())
        .unwrap_or_else(|| to.split('@').next().unwrap_or("there").to_string());
    let topic = followup
        .and_then(|row| row.about.clone())
        .or_else(|| messages.last().map(|message| message.subject.clone()))
        .unwrap_or_else(|| "your last note".to_string());
    let body = match request.kind {
        DraftKind::Nudge => nudge_body(&name, &topic),
        DraftKind::Reply => reply_body(&name, &topic),
    };
    Ok(body)
}

async fn remember_draft(
    store: &impl FollowupStore,
    followup: &Followup,
    record: &DraftRecord,
) -> Result<(), AssistantError> {
    let mut next = followup.clone();
    next.draft_id = Some(record.draft_id.clone());
    next.updated_at = followup.updated_at;
    store
        .transition(
            &followup.account_key,
            &followup.id,
            followup.status,
            next,
            None,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FollowupStatus, MessageView};
    use crate::store::InMemoryStore;
    use crate::testing::FakeMail;
    use chrono::TimeZone;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
            .single()
            .unwrap()
    }

    fn message(body: &str) -> MessageView {
        MessageView {
            message_id: "m1".into(),
            thread_id: "thread-priya".into(),
            from: "Alex <alex@demo.tagos.example>".into(),
            to: "Priya Shah <priya@example.com>".into(),
            subject: "Contract for signature".into(),
            body_text: body.into(),
            received_at: now(),
            folder: "sent".into(),
        }
    }

    async fn followup(store: &InMemoryStore) -> Followup {
        let row = Followup {
            id: "fu-1".into(),
            account_key: "acct".into(),
            counterpart_address: "priya@example.com".into(),
            counterpart_name: Some("Priya".into()),
            thread_id: Some("thread-priya".into()),
            about: Some("the contract".into()),
            created_at: now(),
            due_at: now() + chrono::Duration::days(2),
            tz: "America/New_York".into(),
            baseline_at: now(),
            status: FollowupStatus::Waiting,
            snooze_until: None,
            replied_message_id: None,
            replied_at: None,
            draft_id: None,
            last_checked_at: None,
            updated_at: now(),
        };
        store.insert_followup(row.clone()).await.unwrap();
        row
    }

    #[test]
    fn reply_subject_adds_re_once() {
        assert_eq!(
            reply_subject("Contract for signature"),
            "Re: Contract for signature"
        );
        assert_eq!(
            reply_subject("Re: Contract for signature"),
            "Re: Contract for signature"
        );
        assert_eq!(reply_subject("re: already"), "re: already");
    }

    #[tokio::test]
    async fn nudge_template_is_saved_as_an_unsent_draft() {
        let mut mail = FakeMail::new();
        mail.messages = vec![message("Please sign.")];
        let store = InMemoryStore::new();
        followup(&store).await;
        let prepared = prepare_reply_draft(
            &mail,
            &store,
            "acct",
            DraftRequest {
                followup_id: Some("fu-1".into()),
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
        assert!(!prepared.sent);
        assert!(prepared.requires_user_review);
        assert_eq!(prepared.note, DRAFT_NOTE);
        assert_eq!(prepared.to, "priya@example.com");
        assert_eq!(prepared.subject, "Re: Contract for signature");
        assert!(prepared.body_text.contains("gentle nudge"));
        assert!(prepared.body_text.contains("the contract"));
        let saved = store.get_followup("acct", "fu-1").await.unwrap().unwrap();
        assert_eq!(saved.draft_id.as_deref(), Some(prepared.draft_id.as_str()));
    }

    #[tokio::test]
    async fn recipient_not_allowed_without_confirm_new_recipient() {
        let mut mail = FakeMail::new();
        mail.messages = vec![message("Please sign.")];
        let store = InMemoryStore::new();
        followup(&store).await;
        let err = prepare_reply_draft(
            &mail,
            &store,
            "acct",
            DraftRequest {
                followup_id: Some("fu-1".into()),
                thread_id: None,
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
        assert!(mail.list_drafts(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn confirmed_new_recipient_is_flagged() {
        let mut mail = FakeMail::new();
        mail.messages = vec![message("Please sign.")];
        let store = InMemoryStore::new();
        followup(&store).await;
        let prepared = prepare_reply_draft(
            &mail,
            &store,
            "acct",
            DraftRequest {
                followup_id: Some("fu-1".into()),
                thread_id: None,
                body_text: Some("Hello".into()),
                kind: DraftKind::Reply,
                to: Some("collect@example.net".into()),
                confirm_new_recipient: true,
                subject: Some("Hello".into()),
            },
        )
        .await
        .unwrap();
        assert!(prepared.new_recipient);
        assert!(!prepared.sent);
        assert_eq!(prepared.to, "collect@example.net");
    }

    #[tokio::test]
    async fn quoted_mail_over_five_hundred_chars_is_rejected() {
        let source = "q".repeat(501);
        let mut mail = FakeMail::new();
        mail.messages = vec![message(&source)];
        let store = InMemoryStore::new();
        followup(&store).await;
        let err = prepare_reply_draft(
            &mail,
            &store,
            "acct",
            DraftRequest {
                followup_id: None,
                thread_id: Some("thread-priya".into()),
                body_text: Some(source),
                kind: DraftKind::Reply,
                to: Some("priya@example.com".into()),
                confirm_new_recipient: false,
                subject: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
    }

    #[tokio::test]
    async fn body_over_four_thousand_chars_is_rejected() {
        let mut mail = FakeMail::new();
        mail.messages = vec![message("short")];
        let store = InMemoryStore::new();
        let err = prepare_reply_draft(
            &mail,
            &store,
            "acct",
            DraftRequest {
                followup_id: None,
                thread_id: Some("thread-priya".into()),
                body_text: Some("b".repeat(BODY_TEXT_MAX_CHARS + 1)),
                kind: DraftKind::Reply,
                to: Some("priya@example.com".into()),
                confirm_new_recipient: false,
                subject: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
    }
}
