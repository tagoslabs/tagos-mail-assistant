use crate::backend::MailBackend;
use crate::briefing::{inbox_briefing, Briefing, BriefingQuery, MailCache};
use crate::clock::Clock;
use crate::detector::{check_followups, CheckReport};
use crate::drafts::{prepare_reply_draft, DraftRequest, PreparedDraft};
use crate::errors::AssistantError;
use crate::model::{clamp_limit, Followup, FollowupFilter};
use crate::scheduler::{resolve_followup, take_due_reminders, DueReminder, ResolveAction};
use crate::store::FollowupStore;
use crate::tracker::{await_reply, lazy_check, AwaitReplyOutcome, AwaitReplyRequest};

#[derive(Clone)]
pub struct Assistant<B, S, C> {
    pub backend: B,
    pub store: S,
    pub clock: C,
    cache: MailCache,
}

impl<B, S, C> Assistant<B, S, C>
where
    B: MailBackend,
    S: FollowupStore,
    C: Clock,
{
    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    pub fn new(backend: B, store: S, clock: C) -> Self {
        Self {
            backend,
            store,
            clock,
            cache: MailCache::new(),
        }
    }

    pub async fn await_reply(
        &self,
        account_key: &str,
        request: AwaitReplyRequest,
    ) -> Result<AwaitReplyOutcome, AssistantError> {
        await_reply(
            &self.backend,
            &self.store,
            &self.clock,
            account_key,
            request,
        )
        .await
    }

    pub async fn list_followups(
        &self,
        account_key: &str,
        filter: FollowupFilter,
        limit: Option<u32>,
    ) -> Result<FollowupPage, AssistantError> {
        lazy_check(&self.backend, &self.store, &self.clock, account_key).await?;
        let checked_at = self.clock.now();
        let due_reminders = take_due_reminders(&self.store, account_key, checked_at).await?;
        let mut followups = self.store.list_followups(account_key, filter).await?;
        followups.truncate(clamp_limit(limit) as usize);
        Ok(FollowupPage {
            followups,
            checked_at,
            due_reminders,
        })
    }

    pub async fn check_followups(&self, account_key: &str) -> Result<CheckReport, AssistantError> {
        check_followups(&self.backend, &self.store, &self.clock, account_key).await
    }

    pub async fn resolve_followup(
        &self,
        account_key: &str,
        followup_id: &str,
        action: ResolveAction,
        until: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Followup, AssistantError> {
        resolve_followup(
            &self.store,
            account_key,
            followup_id,
            action,
            until,
            self.clock.now(),
        )
        .await
    }

    pub async fn forget_followups(
        &self,
        account_key: &str,
        confirm: bool,
    ) -> Result<u64, AssistantError> {
        if !confirm {
            return Err(AssistantError::InvalidArgument(
                "forget_followups requires confirm: true".into(),
            ));
        }
        self.store.forget(account_key).await
    }

    pub async fn inbox_briefing(
        &self,
        account_key: &str,
        query: BriefingQuery,
    ) -> Result<Briefing, AssistantError> {
        inbox_briefing(
            &self.backend,
            &self.store,
            &self.clock,
            &self.cache,
            account_key,
            query,
        )
        .await
    }

    pub async fn prepare_reply_draft(
        &self,
        account_key: &str,
        request: DraftRequest,
    ) -> Result<PreparedDraft, AssistantError> {
        prepare_reply_draft(&self.backend, &self.store, account_key, request).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowupPage {
    pub followups: Vec<Followup>,
    pub checked_at: chrono::DateTime<chrono::Utc>,
    pub due_reminders: Vec<DueReminder>,
}
