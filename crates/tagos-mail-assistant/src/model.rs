use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const DEFAULT_TZ: &str = "America/New_York";
pub const READ_LIMIT_DEFAULT: u32 = 20;
pub const READ_LIMIT_MAX: u32 = 50;
pub const BODY_TEXT_MAX_CHARS: usize = 4000;
pub const ABOUT_MAX_CHARS: usize = 200;
pub const SPOKEN_SUBJECT_MAX_CHARS: usize = 60;
pub const QUOTE_MAX_CHARS: usize = 500;
pub const RETENTION_DAYS: i64 = 30;
pub const LAZY_CHECK_SECS: i64 = 60;
pub const SUGGESTED_MIN_AGE_DAYS: i64 = 3;
pub const SUGGESTED_MAX_AGE_DAYS: i64 = 14;
pub const DETECTOR_CALL_BUDGET: u32 = 8;
pub const BRIEFING_CALL_BUDGET: u32 = 12;

pub fn clamp_limit(raw: Option<u32>) -> u32 {
    match raw.unwrap_or(READ_LIMIT_DEFAULT) {
        0 => READ_LIMIT_DEFAULT,
        n if n > READ_LIMIT_MAX => READ_LIMIT_MAX,
        n => n,
    }
}

pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowupStatus {
    Waiting,
    Replied,
    Overdue,
    Snoozed,
    Done,
    Cancelled,
}

impl FollowupStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Replied => "replied",
            Self::Overdue => "overdue",
            Self::Snoozed => "snoozed",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Waiting | Self::Overdue | Self::Snoozed)
    }

    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled | Self::Replied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    ReplyArrived,
    BecameOverdue,
    SnoozeEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowupFilter {
    Active,
    Replied,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Followup {
    pub id: String,
    pub account_key: String,
    pub counterpart_address: String,
    pub counterpart_name: Option<String>,
    pub thread_id: Option<String>,
    pub about: Option<String>,
    pub created_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub tz: String,
    pub baseline_at: DateTime<Utc>,
    pub status: FollowupStatus,
    pub snooze_until: Option<DateTime<Utc>>,
    pub replied_message_id: Option<String>,
    pub replied_at: Option<DateTime<Utc>>,
    pub draft_id: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantEvent {
    pub id: String,
    pub account_key: String,
    pub followup_id: String,
    pub kind: EventKind,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    pub account_key: String,
    pub tz: Option<String>,
    pub last_briefing_at: Option<DateTime<Utc>>,
    pub last_mail_check_at: Option<DateTime<Utc>>,
}

impl Prefs {
    pub fn new(account_key: impl Into<String>) -> Self {
        Self {
            account_key: account_key.into(),
            tz: None,
            last_briefing_at: None,
            last_mail_check_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub address: String,
    pub name: Option<String>,
    pub last_activity: Option<DateTime<Utc>>,
    pub unread_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadView {
    pub thread_id: String,
    pub subject: String,
    pub last_message_at: DateTime<Utc>,
    pub unread_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageView {
    pub message_id: String,
    pub thread_id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body_text: String,
    pub received_at: DateTime<Utc>,
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub message_id: String,
    pub thread_id: String,
    pub subject: String,
    pub from: String,
    pub snippet: String,
    pub folder: String,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewDraft {
    pub to: String,
    pub subject: String,
    pub body_text: String,
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPatch {
    pub to: Option<String>,
    pub subject: Option<String>,
    pub body_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRecord {
    pub draft_id: String,
    pub to: String,
    pub subject: String,
    pub body_text: String,
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub address: String,
    pub name: Option<String>,
    pub last_activity: Option<DateTime<Utc>>,
}

/// ```
/// use tagos_mail_assistant::model::FollowupStatus;
/// assert_eq!(FollowupStatus::Waiting.as_str(), "waiting");
/// assert!(FollowupStatus::Overdue.is_active());
/// assert!(!FollowupStatus::Replied.is_active());
/// ```
#[allow(dead_code)]
fn _followup_status_doctest() {}
