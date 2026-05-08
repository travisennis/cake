use crate::clients::retry::RetryStatus;
use crate::clients::types::{ConversationItem, SessionRecord, StreamRecord};

type StreamingCallback = Box<dyn Fn(&str) + Send + Sync>;
type PersistCallback = Box<dyn FnMut(&SessionRecord) -> anyhow::Result<()> + Send + Sync>;
type ProgressCallback = Box<dyn Fn(&ConversationItem) + Send + Sync>;
type RetryCallback = Box<dyn Fn(&RetryStatus) + Send + Sync>;

#[derive(Default)]
pub(super) struct AgentObserver {
    streaming: Option<StreamingCallback>,
    persist: Option<PersistCallback>,
    progress: Option<ProgressCallback>,
    retry: Option<RetryCallback>,
}

impl AgentObserver {
    pub(super) fn set_streaming_json(&mut self, callback: impl Fn(&str) + Send + Sync + 'static) {
        self.streaming = Some(Box::new(callback));
    }

    pub(super) fn set_persist_callback(
        &mut self,
        callback: impl FnMut(&SessionRecord) -> anyhow::Result<()> + Send + Sync + 'static,
    ) {
        self.persist = Some(Box::new(callback));
    }

    pub(super) fn set_progress_callback(
        &mut self,
        callback: impl Fn(&ConversationItem) + Send + Sync + 'static,
    ) {
        self.progress = Some(Box::new(callback));
    }

    pub(super) fn set_retry_callback(
        &mut self,
        callback: impl Fn(&RetryStatus) + Send + Sync + 'static,
    ) {
        self.retry = Some(Box::new(callback));
    }

    pub(super) fn report_progress(&self, item: &ConversationItem) {
        if let Some(ref callback) = self.progress {
            callback(item);
        }
    }

    pub(super) fn report_retry(&self, status: &RetryStatus) {
        if let Some(ref callback) = self.retry {
            callback(status);
        }
    }

    pub(super) fn stream_record(&mut self, record: StreamRecord) -> anyhow::Result<()> {
        let stream_json = self
            .streaming
            .as_ref()
            .and_then(|_| serde_json::to_string(&record).ok());
        let session_record = SessionRecord::from(record);
        if let Some(ref mut callback) = self.persist {
            callback(&session_record)?;
        }
        if let Some(ref callback) = self.streaming
            && let Some(json) = stream_json
        {
            callback(&json);
        }
        Ok(())
    }

    pub(super) fn persist_record(&mut self, record: &SessionRecord) -> anyhow::Result<()> {
        if let Some(ref mut callback) = self.persist {
            callback(record)?;
        }
        Ok(())
    }

    pub(super) fn stream_item(&mut self, item: &ConversationItem) -> anyhow::Result<()> {
        self.stream_record(StreamRecord::from_conversation_item(item))
    }
}
