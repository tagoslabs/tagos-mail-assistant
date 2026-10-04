/// Stable tool-error codes. Display is the code only, never a stack trace.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssistantError {
    #[error("invalid_argument")]
    InvalidArgument(String),
    #[error("not_found")]
    NotFound,
    #[error("recipient_not_allowed")]
    RecipientNotAllowed,
    #[error("rate_limited")]
    RateLimited,
    #[error("backend_unavailable")]
    BackendUnavailable,
}

impl AssistantError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArgument(_) => "invalid_argument",
            Self::NotFound => "not_found",
            Self::RecipientNotAllowed => "recipient_not_allowed",
            Self::RateLimited => "rate_limited",
            Self::BackendUnavailable => "backend_unavailable",
        }
    }

    /// Short host-facing text. No paths and no panic payload.
    pub fn message(&self) -> String {
        match self {
            Self::InvalidArgument(reason) => reason.clone(),
            Self::NotFound => "not found".to_string(),
            Self::RecipientNotAllowed => "recipient is not on this thread".to_string(),
            Self::RateLimited => "too many mailbox reads; the rest was deferred".to_string(),
            Self::BackendUnavailable => "mailbox is unavailable".to_string(),
        }
    }
}
