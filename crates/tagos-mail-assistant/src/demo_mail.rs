use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::backend::MailBackend;
use crate::errors::AssistantError;
use crate::model::{DraftPatch, DraftRecord, MessageView, NewDraft, Person, SearchHit, ThreadView};
use crate::untrusted::addr_spec;

#[derive(Debug, Clone, Deserialize)]
struct StoredMessage {
    message_id: String,
    thread_id: String,
    counterpart: String,
    counterpart_name: String,
    from: String,
    to: String,
    subject: String,
    body_text: String,
    received_at: DateTime<Utc>,
    folder: String,
    #[serde(default)]
    unread: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Scenario {
    thread_id: String,
    from: String,
    to: String,
    subject: String,
    body_text: String,
}

#[derive(Debug, Deserialize)]
struct MailboxFile {
    owner_address: String,
    owner_name: String,
    messages: Vec<StoredMessage>,
}

#[derive(Debug, Deserialize)]
struct ScenarioFile {
    scenarios: HashMap<String, Scenario>,
}

struct DemoInner {
    owner_address: String,
    owner_name: String,
    original: Vec<StoredMessage>,
    messages: Mutex<Vec<StoredMessage>>,
    drafts: Mutex<Vec<DraftRecord>>,
    scenarios: HashMap<String, Scenario>,
}

#[derive(Clone)]
pub struct DemoMail {
    inner: Arc<DemoInner>,
}

impl DemoMail {
    pub fn from_json(mailbox_json: &str, scenario_json: &str) -> Result<Self, AssistantError> {
        let file: MailboxFile = serde_json::from_str(mailbox_json)
            .map_err(|_| AssistantError::InvalidArgument("mailbox json is invalid".into()))?;
        let scenarios: ScenarioFile = serde_json::from_str(scenario_json)
            .map_err(|_| AssistantError::InvalidArgument("scenario json is invalid".into()))?;
        Ok(Self {
            inner: Arc::new(DemoInner {
                owner_address: file.owner_address,
                owner_name: file.owner_name,
                original: file.messages.clone(),
                messages: Mutex::new(file.messages),
                drafts: Mutex::new(Vec::new()),
                scenarios: scenarios.scenarios,
            }),
        })
    }

    pub fn deliver(
        &self,
        scenario: &str,
        at: DateTime<Utc>,
    ) -> Result<MessageView, AssistantError> {
        let script = self
            .inner
            .scenarios
            .get(scenario)
            .cloned()
            .ok_or_else(|| AssistantError::InvalidArgument("unknown scenario".into()))?;
        let mut messages = self.inner.messages.lock().expect("messages");
        let counterpart = messages
            .iter()
            .find(|message| message.thread_id == script.thread_id)
            .map(|message| {
                (
                    message.counterpart.clone(),
                    message.counterpart_name.clone(),
                )
            })
            .ok_or(AssistantError::NotFound)?;
        let stored = StoredMessage {
            message_id: crate::model::new_id(),
            thread_id: script.thread_id,
            counterpart: counterpart.0,
            counterpart_name: counterpart.1,
            from: script.from,
            to: script.to,
            subject: script.subject,
            body_text: script.body_text,
            received_at: at,
            folder: "inbox".into(),
            unread: true,
        };
        let view = to_view(&stored);
        messages.push(stored);
        Ok(view)
    }

    pub fn reset(&self) {
        *self.inner.messages.lock().expect("messages") = self.inner.original.clone();
        self.inner.drafts.lock().expect("drafts").clear();
    }

    fn messages(&self) -> Vec<StoredMessage> {
        self.inner.messages.lock().expect("messages").clone()
    }
}

fn to_view(message: &StoredMessage) -> MessageView {
    MessageView {
        message_id: message.message_id.clone(),
        thread_id: message.thread_id.clone(),
        from: message.from.clone(),
        to: message.to.clone(),
        subject: message.subject.clone(),
        body_text: message.body_text.clone(),
        received_at: message.received_at,
        folder: message.folder.clone(),
    }
}

fn people_from(messages: &[StoredMessage]) -> Vec<Person> {
    let mut grouped: Vec<Person> = Vec::new();
    for message in messages {
        let unread = i64::from(message.unread);
        if let Some(person) = grouped
            .iter_mut()
            .find(|person| person.address.eq_ignore_ascii_case(&message.counterpart))
        {
            person.unread_count += unread;
            if person
                .last_activity
                .is_none_or(|at| message.received_at > at)
            {
                person.last_activity = Some(message.received_at);
            }
            if person.name.is_none() {
                person.name = Some(message.counterpart_name.clone());
            }
        } else {
            grouped.push(Person {
                address: message.counterpart.clone(),
                name: Some(message.counterpart_name.clone()),
                last_activity: Some(message.received_at),
                unread_count: unread,
            });
        }
    }
    grouped.sort_by_key(|person| {
        (
            std::cmp::Reverse(person.unread_count > 0),
            std::cmp::Reverse(person.last_activity),
            person.address.clone(),
        )
    });
    grouped
}

#[async_trait]
impl MailBackend for DemoMail {
    fn owner_address(&self) -> &str {
        &self.inner.owner_address
    }

    fn owner_name(&self) -> &str {
        &self.inner.owner_name
    }

    async fn list_people(
        &self,
        query: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Person>, AssistantError> {
        let needle = query.unwrap_or("").to_ascii_lowercase();
        let mut people = people_from(&self.messages());
        people.retain(|person| {
            needle.is_empty()
                || person.address.to_ascii_lowercase().contains(&needle)
                || person
                    .name
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(&needle)
        });
        people.truncate(limit as usize);
        Ok(people)
    }

    async fn list_contexts(
        &self,
        address: &str,
        limit: u32,
    ) -> Result<Vec<ThreadView>, AssistantError> {
        let spec = addr_spec(address);
        let messages = self.messages();
        let mut threads: Vec<ThreadView> = Vec::new();
        for message in messages
            .iter()
            .filter(|message| message.counterpart.eq_ignore_ascii_case(&spec))
        {
            if let Some(thread) = threads
                .iter_mut()
                .find(|thread| thread.thread_id == message.thread_id)
            {
                if message.received_at > thread.last_message_at {
                    thread.last_message_at = message.received_at;
                    thread.subject = message.subject.clone();
                }
                thread.unread_count += i64::from(message.unread);
            } else {
                threads.push(ThreadView {
                    thread_id: message.thread_id.clone(),
                    subject: message.subject.clone(),
                    last_message_at: message.received_at,
                    unread_count: i64::from(message.unread),
                });
            }
        }
        threads.sort_by_key(|thread| {
            (
                std::cmp::Reverse(thread.unread_count > 0),
                std::cmp::Reverse(thread.last_message_at),
                thread.thread_id.clone(),
            )
        });
        threads.truncate(limit as usize);
        Ok(threads)
    }

    async fn get_thread(
        &self,
        thread_id: &str,
        limit: u32,
    ) -> Result<Vec<MessageView>, AssistantError> {
        let mut rows: Vec<_> = self
            .messages()
            .into_iter()
            .filter(|message| message.thread_id == thread_id)
            .collect();
        if rows.is_empty() {
            return Err(AssistantError::NotFound);
        }
        rows.sort_by_key(|message| message.received_at);
        rows.truncate(limit as usize);
        Ok(rows.iter().map(to_view).collect())
    }

    async fn search_messages(
        &self,
        query: &str,
        thread_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>, AssistantError> {
        if query.trim().is_empty() {
            return Err(AssistantError::InvalidArgument("query required".into()));
        }
        let words: Vec<String> = query
            .split_whitespace()
            .map(|word| word.to_ascii_lowercase())
            .collect();
        let mut hits = Vec::new();
        for message in self.messages() {
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
                    message_id: message.message_id,
                    thread_id: message.thread_id,
                    subject: message.subject,
                    from: message.from,
                    snippet: message.body_text.chars().take(500).collect(),
                    folder: message.folder,
                    received_at: message.received_at,
                });
            }
        }
        hits.truncate(limit as usize);
        Ok(hits)
    }

    async fn create_draft(&self, draft: NewDraft) -> Result<DraftRecord, AssistantError> {
        let record = DraftRecord {
            draft_id: crate::model::new_id(),
            to: draft.to,
            subject: draft.subject,
            body_text: draft.body_text,
            thread_id: draft.thread_id,
        };
        self.inner
            .drafts
            .lock()
            .expect("drafts")
            .push(record.clone());
        Ok(record)
    }

    async fn update_draft(
        &self,
        draft_id: &str,
        patch: DraftPatch,
    ) -> Result<DraftRecord, AssistantError> {
        let mut drafts = self.inner.drafts.lock().expect("drafts");
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
        let mut drafts = self.inner.drafts.lock().expect("drafts");
        let before = drafts.len();
        drafts.retain(|row| row.draft_id != draft_id);
        if drafts.len() == before {
            return Err(AssistantError::NotFound);
        }
        Ok(())
    }

    async fn list_drafts(&self, limit: u32) -> Result<Vec<DraftRecord>, AssistantError> {
        let drafts = self.inner.drafts.lock().expect("drafts");
        Ok(drafts.iter().take(limit as usize).cloned().collect())
    }
}
