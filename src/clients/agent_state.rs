use crate::clients::types::{ConversationItem, Usage};
use crate::models::{Message, Role};

#[derive(Debug)]
pub(super) struct ConversationState {
    history: Vec<ConversationItem>,
}

impl ConversationState {
    pub(super) fn new(initial_messages: &[(Role, String)]) -> Self {
        let timestamp = chrono::Utc::now().to_rfc3339();
        Self {
            history: initial_messages
                .iter()
                .map(|(role, content)| ConversationItem::Message {
                    role: *role,
                    content: content.clone(),
                    id: None,
                    status: None,
                    timestamp: Some(timestamp.clone()),
                })
                .collect(),
        }
    }

    pub(super) fn history(&self) -> &[ConversationItem] {
        &self.history
    }

    pub(super) fn append_developer_context(&mut self, contexts: Vec<String>) {
        let timestamp = chrono::Utc::now().to_rfc3339();
        for content in contexts {
            if content.is_empty() {
                continue;
            }
            self.history.push(ConversationItem::Message {
                role: Role::Developer,
                content,
                id: None,
                status: None,
                timestamp: Some(timestamp.clone()),
            });
        }
    }

    pub(super) fn with_restored_history(&mut self, messages: Vec<ConversationItem>) {
        debug_assert!(
            !self.history.is_empty(),
            "with_history requires Agent::new() to have set initial prompt messages"
        );
        let first_non_prompt = messages
            .iter()
            .position(|item| {
                !matches!(
                    item,
                    ConversationItem::Message {
                        role: Role::System | Role::Developer,
                        ..
                    }
                )
            })
            .unwrap_or(messages.len());
        self.history
            .extend(messages.into_iter().skip(first_non_prompt));
    }

    pub(super) fn push_user_message(&mut self, content: String) -> ConversationItem {
        let item = ConversationItem::Message {
            role: Role::User,
            content,
            id: None,
            status: None,
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
        };
        self.history.push(item.clone());
        item
    }

    pub(super) fn extend_turn_items(&mut self, items: Vec<ConversationItem>) {
        self.history.extend(items);
    }

    pub(super) fn push_tool_output(&mut self, call_id: String, output: String) -> ConversationItem {
        let item = ConversationItem::FunctionCallOutput {
            call_id,
            output,
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
        };
        self.history.push(item.clone());
        item
    }

    pub(super) fn resolve_assistant_message(&self) -> Message {
        resolve_assistant_message(&self.history)
    }

    #[cfg(test)]
    pub(super) const fn history_mut(&mut self) -> &mut Vec<ConversationItem> {
        &mut self.history
    }
}

pub(super) const fn accumulate_usage(
    total_usage: &mut Usage,
    turn_count: &mut u32,
    turn_usage: Option<&Usage>,
) {
    if let Some(usage) = turn_usage {
        total_usage.input_tokens += usage.input_tokens;
        total_usage.input_tokens_details.cached_tokens += usage.input_tokens_details.cached_tokens;
        total_usage.output_tokens += usage.output_tokens;
        total_usage.output_tokens_details.reasoning_tokens +=
            usage.output_tokens_details.reasoning_tokens;
        total_usage.total_tokens += usage.total_tokens;
        *turn_count += 1;
    }
}

fn resolve_assistant_message(items: &[ConversationItem]) -> Message {
    if let Some(msg) = items.iter().rev().find_map(|item| {
        if let ConversationItem::Message {
            role: Role::Assistant,
            content,
            ..
        } = item
        {
            Some(Message {
                role: Role::Assistant,
                content: content.clone(),
            })
        } else {
            None
        }
    }) {
        return msg;
    }

    let content = if items.is_empty() {
        "No response was received from the model.".to_string()
    } else if items
        .iter()
        .any(|item| matches!(item, ConversationItem::Reasoning { .. }))
    {
        "The model's response was incomplete. The task may have been partially completed but was cut off during reasoning.".to_string()
    } else {
        "The model's response was incomplete. No final message was received.".to_string()
    };

    Message {
        role: Role::Assistant,
        content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_assistant_message_with_assistant_message() {
        let items = vec![ConversationItem::Message {
            role: Role::Assistant,
            content: "Hello!".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        }];
        let msg = resolve_assistant_message(&items);
        assert_eq!(msg.content, "Hello!");
    }

    #[test]
    fn resolve_assistant_message_truncated_with_reasoning() {
        let items = vec![ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: None,
            content: None,
            timestamp: None,
        }];
        let msg = resolve_assistant_message(&items);
        assert!(msg.content.contains("cut off during reasoning"));
    }

    #[test]
    fn resolve_assistant_message_no_output_items() {
        let items: Vec<ConversationItem> = vec![];
        let msg = resolve_assistant_message(&items);
        assert_eq!(msg.content, "No response was received from the model.");
    }

    #[test]
    fn resolve_assistant_message_items_but_no_message_or_reasoning() {
        let items = vec![ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: "{}".to_string(),
            timestamp: None,
        }];
        let msg = resolve_assistant_message(&items);
        assert_eq!(
            msg.content,
            "The model's response was incomplete. No final message was received."
        );
    }
}
