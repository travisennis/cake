use crate::types::{ConversationItem, SessionRecord, StreamRecord};

type StreamingCallback = Box<dyn Fn(&str) + Send + Sync>;
type PersistCallback = Box<dyn FnMut(&SessionRecord) -> anyhow::Result<()> + Send + Sync>;

#[derive(Default)]
pub(super) struct AgentObserver {
    streaming: Option<StreamingCallback>,
    persist: Option<PersistCallback>,
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

    pub(super) fn stream_record(&mut self, record: StreamRecord) -> anyhow::Result<()> {
        let stream_json = self.streaming.as_ref().and_then(|_| {
            serde_json::to_string(&record)
                .inspect_err(|e| tracing::warn!("Stream serialization failed: {e}"))
                .ok()
        });
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
