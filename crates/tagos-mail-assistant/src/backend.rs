use async_trait::async_trait;

use crate::errors::AssistantError;
use crate::model::{DraftPatch, DraftRecord, MessageView, NewDraft, Person, SearchHit, ThreadView};

/// Mailbox reads and drafts. There is no send method on purpose.
#[async_trait]
pub trait MailBackend: Send + Sync {
    fn owner_address(&self) -> &str;
    fn owner_name(&self) -> &str;

    async fn list_people(
        &self,
        query: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Person>, AssistantError>;

    async fn list_contexts(
        &self,
        address: &str,
        limit: u32,
    ) -> Result<Vec<ThreadView>, AssistantError>;

    async fn get_thread(
        &self,
        thread_id: &str,
        limit: u32,
    ) -> Result<Vec<MessageView>, AssistantError>;

    async fn search_messages(
        &self,
        query: &str,
        thread_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>, AssistantError>;

    async fn create_draft(&self, draft: NewDraft) -> Result<DraftRecord, AssistantError>;

    async fn update_draft(
        &self,
        draft_id: &str,
        patch: DraftPatch,
    ) -> Result<DraftRecord, AssistantError>;

    async fn delete_draft(&self, draft_id: &str) -> Result<(), AssistantError>;

    async fn list_drafts(&self, limit: u32) -> Result<Vec<DraftRecord>, AssistantError>;
}

#[cfg(test)]
mod tests {
    #[test]
    fn mail_backend_trait_has_no_send_method() {
        let src = include_str!("backend.rs");
        let banned_fn = ["fn ", "send"].concat();
        let banned_message = ["send", "_message"].concat();
        let banned_draft = ["send", "_draft"].concat();
        assert!(
            !src.contains(&banned_fn),
            "MailBackend must not grow a send method"
        );
        assert!(!src.contains(&banned_message));
        assert!(!src.contains(&banned_draft));
    }
}
