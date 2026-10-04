use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::backend::MailBackend;
use crate::errors::AssistantError;
use crate::model::{DraftPatch, DraftRecord, MessageView, NewDraft, Person, SearchHit, ThreadView};
use async_trait::async_trait;

pub struct FakeMail {
    pub owner_address: String,
    pub owner_name: String,
    pub people: Vec<Person>,
    pub contexts: Vec<(String, ThreadView)>,
    pub messages: Vec<MessageView>,
    pub calls: AtomicU32,
    pub fail_after: Option<u32>,
    drafts: Mutex<Vec<DraftRecord>>,
}

impl FakeMail {
    pub fn new() -> Self {
        Self {
            owner_address: "alex@demo.tagos.example".into(),
            owner_name: "Alex".into(),
            people: Vec::new(),
            contexts: Vec::new(),
            messages: Vec::new(),
            calls: AtomicU32::new(0),
            fail_after: None,
            drafts: Mutex::new(Vec::new()),
        }
    }

    fn gate(&self) -> Result<(), AssistantError> {
        let seen = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_after.is_some_and(|limit| seen >= limit) {
            return Err(AssistantError::RateLimited);
        }
        Ok(())
    }
}

impl Default for FakeMail {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MailBackend for FakeMail {
    fn owner_address(&self) -> &str {
        &self.owner_address
    }

    fn owner_name(&self) -> &str {
        &self.owner_name
    }

    async fn list_people(
        &self,
        query: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Person>, AssistantError> {
        self.gate()?;
        let needle = query.unwrap_or("").to_ascii_lowercase();
        let mut rows: Vec<_> = self
            .people
            .iter()
            .filter(|person| {
                needle.is_empty()
                    || person.address.to_ascii_lowercase().contains(&needle)
                    || person
                        .name
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(&needle)
            })
            .cloned()
            .collect();
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn list_contexts(
        &self,
        address: &str,
        limit: u32,
    ) -> Result<Vec<ThreadView>, AssistantError> {
        self.gate()?;
        let mut rows: Vec<_> = self
            .contexts
            .iter()
            .filter(|(owner, _)| owner.eq_ignore_ascii_case(address))
            .map(|(_, thread)| thread.clone())
            .collect();
        rows.sort_by_key(|thread| std::cmp::Reverse(thread.last_message_at));
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn get_thread(
        &self,
        thread_id: &str,
        limit: u32,
    ) -> Result<Vec<MessageView>, AssistantError> {
        self.gate()?;
        let mut rows: Vec<_> = self
            .messages
            .iter()
            .filter(|message| message.thread_id == thread_id)
            .cloned()
            .collect();
        rows.sort_by_key(|message| message.received_at);
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn search_messages(
        &self,
        query: &str,
        thread_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>, AssistantError> {
        self.gate()?;
        if query.trim().is_empty() {
            return Err(AssistantError::InvalidArgument("query required".into()));
        }
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_ascii_lowercase())
            .collect();
        let mut hits = Vec::new();
        for message in &self.messages {
            if let Some(thread) = thread_id {
                if message.thread_id != thread {
                    continue;
                }
            } else if message.folder.eq_ignore_ascii_case("trash") {
                continue;
            }
            let haystack = if thread_id.is_some() {
                message.body_text.to_ascii_lowercase()
            } else {
                format!(
                    "{} {} {} {}",
                    message.subject, message.from, message.to, message.body_text
                )
                .to_ascii_lowercase()
            };
            if words.iter().all(|word| haystack.contains(word)) {
                hits.push(SearchHit {
                    message_id: message.message_id.clone(),
                    thread_id: message.thread_id.clone(),
                    subject: message.subject.clone(),
                    from: message.from.clone(),
                    snippet: message.body_text.chars().take(500).collect(),
                    folder: message.folder.clone(),
                    received_at: message.received_at,
                });
            }
        }
        hits.truncate(limit as usize);
        Ok(hits)
    }

    async fn create_draft(&self, draft: NewDraft) -> Result<DraftRecord, AssistantError> {
        self.gate()?;
        let record = DraftRecord {
            draft_id: crate::model::new_id(),
            to: draft.to,
            subject: draft.subject,
            body_text: draft.body_text,
            thread_id: draft.thread_id,
        };
        self.drafts.lock().expect("drafts").push(record.clone());
        Ok(record)
    }

    async fn update_draft(
        &self,
        draft_id: &str,
        patch: DraftPatch,
    ) -> Result<DraftRecord, AssistantError> {
        self.gate()?;
        let mut drafts = self.drafts.lock().expect("drafts");
        let Some(row) = drafts.iter_mut().find(|row| row.draft_id == draft_id) else {
            return Err(AssistantError::NotFound);
        };
        if let Some(to) = patch.to {
            row.to = to;
        }
        if let Some(subject) = patch.subject {
            row.subject = subject;
        }
        if let Some(body_text) = patch.body_text {
            row.body_text = body_text;
        }
        Ok(row.clone())
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), AssistantError> {
        self.gate()?;
        let mut drafts = self.drafts.lock().expect("drafts");
        let before = drafts.len();
        drafts.retain(|row| row.draft_id != draft_id);
        if drafts.len() == before {
            return Err(AssistantError::NotFound);
        }
        Ok(())
    }

    async fn list_drafts(&self, limit: u32) -> Result<Vec<DraftRecord>, AssistantError> {
        self.gate()?;
        let drafts = self.drafts.lock().expect("drafts");
        Ok(drafts.iter().take(limit as usize).cloned().collect())
    }
}
