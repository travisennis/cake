use crate::types::{ConversationItem, Role, Usage};

#[derive(Debug)]
pub(super) struct ConversationState {
    history: Vec<ConversationItem>,
    /// Repair outputs synthesized by [`Self::with_restored_history`] that have
    /// not yet been streamed and persisted.
    pending_repairs: Vec<ConversationItem>,
}

impl ConversationState {
    pub(super) fn new(initial_messages: &[(Role, String)]) -> Self {
        let timestamp = chrono::Utc::now();
        Self {
            history: initial_messages
                .iter()
                .map(|(role, content)| ConversationItem::Message {
                    role: *role,
                    content: content.clone(),
                    id: None,
                    status: None,
                    timestamp: Some(timestamp),
                })
                .collect(),
            pending_repairs: Vec::new(),
        }
    }

    pub(super) fn history(&self) -> &[ConversationItem] {
        &self.history
    }

    pub(super) fn append_developer_context(&mut self, contexts: Vec<String>) {
        let timestamp = chrono::Utc::now();
        for content in contexts {
            if content.is_empty() {
                continue;
            }
            self.history.push(ConversationItem::Message {
                role: Role::Developer,
                content,
                id: None,
                status: None,
                timestamp: Some(timestamp),
            });
        }
    }

    /// Restore a persisted conversation, repairing tool calls the previous
    /// process left without an output.
    ///
    /// A process that ends between persisting a `FunctionCall` and persisting
    /// its `FunctionCallOutput` leaves history that providers reject, so each
    /// unmatched call gains an explicit failed output before the next request.
    /// The repair items are also queued for persistence so later restores see
    /// the same well-formed history.
    pub(super) fn with_restored_history(
        &mut self,
        messages: Vec<ConversationItem>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
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
        let repairs = repair_items_for_incomplete_calls(
            messages.get(first_non_prompt..).unwrap_or_default(),
        )?;
        self.history
            .extend(messages.into_iter().skip(first_non_prompt));
        self.history.extend(repairs.iter().cloned());
        self.pending_repairs.extend(repairs);
        Ok(())
    }

    /// Take the repair outputs awaiting persistence, leaving none behind.
    pub(super) fn take_pending_repairs(&mut self) -> Vec<ConversationItem> {
        std::mem::take(&mut self.pending_repairs)
    }

    pub(super) fn push_user_message(&mut self, content: String) -> ConversationItem {
        let item = ConversationItem::Message {
            role: Role::User,
            content,
            id: None,
            status: None,
            timestamp: Some(chrono::Utc::now()),
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
            timestamp: Some(chrono::Utc::now()),
        };
        self.history.push(item.clone());
        item
    }

    /// Resolve the final assistant message among items at or after `start`.
    ///
    /// Scoping to the current turn keeps a resumed or multi-turn history's
    /// earlier assistant messages from masking a cut-off in the current turn.
    pub(super) fn resolve_assistant_message_from(&self, start: usize) -> Option<String> {
        resolve_assistant_message(self.history.get(start..).unwrap_or_default())
    }

    #[cfg(test)]
    pub(super) const fn history_mut(&mut self) -> &mut Vec<ConversationItem> {
        &mut self.history
    }
}

pub(super) const fn accumulate_usage(total_usage: &mut Usage, turn_usage: Option<&Usage>) {
    if let Some(usage) = turn_usage {
        total_usage.input_tokens += usage.input_tokens;
        total_usage.input_tokens_details.cached_tokens += usage.input_tokens_details.cached_tokens;
        total_usage.input_tokens_details.cache_write_tokens +=
            usage.input_tokens_details.cache_write_tokens;
        total_usage.output_tokens += usage.output_tokens;
        total_usage.output_tokens_details.reasoning_tokens +=
            usage.output_tokens_details.reasoning_tokens;
        total_usage.total_tokens += usage.total_tokens;
    }
}

/// Model-visible output for a function call the previous process abandoned.
///
/// The wording is part of what the model sees on resume; keep it stable.
fn incomplete_tool_call_repair_output(name: &str, call_id: &str) -> String {
    format!(
        "not executed: the previous cake process ended before {name}({call_id}) \
         recorded a result. Assume the tool did not run, and call it again if \
         its result is still needed."
    )
}

/// Build failed outputs for every function call left unmatched by a prior run.
///
/// Pairing walks `items` in order: a call opens, and the next output carrying
/// its `call_id` closes it. Calls still open at the end are the abandoned ones.
///
/// # Errors
///
/// Returns an error when pairing is ambiguous — two calls with the same
/// `call_id` open at once, or an output whose `call_id` has no open call —
/// rather than guessing which call an output belongs to.
fn repair_items_for_incomplete_calls(
    items: &[ConversationItem],
) -> anyhow::Result<Vec<ConversationItem>> {
    let mut open: Vec<(&str, &str)> = Vec::new();
    for item in items {
        match item {
            ConversationItem::FunctionCall { call_id, name, .. } => {
                anyhow::ensure!(
                    !open.iter().any(|(open_id, _)| *open_id == call_id),
                    "Corrupt session history: two unfinished function calls share \
                     call_id '{call_id}'; cannot determine which call an output \
                     belongs to."
                );
                open.push((call_id, name));
            },
            ConversationItem::FunctionCallOutput { call_id, .. } => {
                let position = open
                    .iter()
                    .position(|(open_id, _)| *open_id == call_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Corrupt session history: function call output for \
                             call_id '{call_id}' has no preceding unmatched \
                             function call."
                        )
                    })?;
                open.remove(position);
            },
            ConversationItem::Message { .. } | ConversationItem::Reasoning { .. } => {},
        }
    }

    let timestamp = chrono::Utc::now();
    Ok(open
        .into_iter()
        .map(|(call_id, name)| ConversationItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: incomplete_tool_call_repair_output(name, call_id),
            timestamp: Some(timestamp),
        })
        .collect())
}

fn resolve_assistant_message(items: &[ConversationItem]) -> Option<String> {
    items.iter().rev().find_map(|item| {
        if let ConversationItem::Message {
            role: Role::Assistant,
            content,
            ..
        } = item
            && !content.trim().is_empty()
        {
            Some(content.clone())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReasoningSummary;

    #[test]
    fn resolve_assistant_message_with_assistant_message() {
        let items = vec![ConversationItem::Message {
            role: Role::Assistant,
            content: "Hello!".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        }];
        let content = resolve_assistant_message(&items);
        assert_eq!(content, Some("Hello!".to_string()));
    }

    #[test]
    fn resolve_assistant_message_truncated_with_reasoning() {
        let items = vec![ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: Some(vec![ReasoningSummary::summary_text("thinking...")]),
            encrypted_content: None,
            content: None,
            timestamp: None,
        }];
        let content = resolve_assistant_message(&items);
        assert!(content.is_none());
    }

    #[test]
    fn resolve_assistant_message_no_output_items() {
        let items: Vec<ConversationItem> = vec![];
        let content = resolve_assistant_message(&items);
        assert!(content.is_none());
    }

    #[test]
    fn resolve_assistant_message_ignores_empty_assistant_message() {
        let items = vec![assistant_message(" \n")];
        let content = resolve_assistant_message(&items);
        assert!(content.is_none());
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
        let content = resolve_assistant_message(&items);
        assert!(content.is_none());
    }

    fn assistant_message(content: &str) -> ConversationItem {
        ConversationItem::Message {
            role: Role::Assistant,
            content: content.to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        }
    }

    #[test]
    fn resolve_assistant_message_from_ignores_prior_turn_messages() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state.extend_turn_items(vec![assistant_message("prior answer")]);
        let turn_start = state.history().len();

        assert!(state.resolve_assistant_message_from(turn_start).is_none());
        assert_eq!(
            state.resolve_assistant_message_from(0),
            Some("prior answer".to_string())
        );
    }

    #[test]
    fn resolve_assistant_message_from_finds_current_turn_message() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state.extend_turn_items(vec![assistant_message("prior answer")]);
        let turn_start = state.history().len();
        state.extend_turn_items(vec![assistant_message("current answer")]);

        assert_eq!(
            state.resolve_assistant_message_from(turn_start),
            Some("current answer".to_string())
        );
    }

    #[test]
    fn resolve_assistant_message_from_past_end_is_none() {
        let state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        assert!(state.resolve_assistant_message_from(10).is_none());
    }

    fn user_message(content: &str) -> ConversationItem {
        ConversationItem::Message {
            role: Role::User,
            content: content.to_string(),
            id: None,
            status: None,
            timestamp: None,
        }
    }

    fn function_call(call_id: &str, name: &str) -> ConversationItem {
        ConversationItem::FunctionCall {
            id: format!("fc-{call_id}"),
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
            timestamp: None,
        }
    }

    fn function_call_output(call_id: &str, output: &str) -> ConversationItem {
        ConversationItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: output.to_string(),
            timestamp: None,
        }
    }

    fn repaired_call_ids(items: &[ConversationItem]) -> Vec<&str> {
        items
            .iter()
            .map(|item| match item {
                ConversationItem::FunctionCallOutput { call_id, .. } => call_id.as_str(),
                other => panic!("expected a function call output, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn repair_closes_a_single_unmatched_call() {
        let items = vec![
            user_message("list files"),
            assistant_message("running"),
            function_call("call-1", "Bash"),
        ];

        let repairs = repair_items_for_incomplete_calls(&items).unwrap();

        assert_eq!(repairs.len(), 1);
        let ConversationItem::FunctionCallOutput {
            call_id, output, ..
        } = &repairs[0]
        else {
            panic!("expected a function call output");
        };
        assert_eq!(call_id, "call-1");
        assert_eq!(
            output,
            "not executed: the previous cake process ended before Bash(call-1) \
             recorded a result. Assume the tool did not run, and call it again \
             if its result is still needed."
        );
    }

    #[test]
    fn repair_closes_every_unmatched_call_in_issue_order() {
        let items = vec![
            user_message("do two things"),
            function_call("call-1", "Bash"),
            function_call("call-2", "Read"),
        ];

        let repairs = repair_items_for_incomplete_calls(&items).unwrap();

        assert_eq!(repaired_call_ids(&repairs), vec!["call-1", "call-2"]);
    }

    #[test]
    fn repair_keeps_matched_calls_and_closes_only_the_unmatched_one() {
        let items = vec![
            user_message("do two things"),
            function_call("call-1", "Bash"),
            function_call("call-2", "Read"),
            function_call_output("call-1", "ok"),
        ];

        let repairs = repair_items_for_incomplete_calls(&items).unwrap();

        assert_eq!(repaired_call_ids(&repairs), vec!["call-2"]);
    }

    #[test]
    fn repair_leaves_fully_matched_history_unchanged() {
        let items = vec![
            user_message("list files"),
            function_call("call-1", "Bash"),
            function_call_output("call-1", "ok"),
            assistant_message("done"),
        ];

        assert!(
            repair_items_for_incomplete_calls(&items)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn repair_ignores_interleaved_non_tool_items() {
        let items = vec![
            user_message("investigate"),
            ConversationItem::Reasoning {
                id: "r-1".to_string(),
                summary: Some(vec![ReasoningSummary::summary_text("thinking")]),
                encrypted_content: None,
                content: None,
                timestamp: None,
            },
            assistant_message("looking"),
            function_call("call-1", "Bash"),
            function_call_output("call-1", "ok"),
            assistant_message("still looking"),
            function_call("call-2", "Read"),
        ];

        let repairs = repair_items_for_incomplete_calls(&items).unwrap();

        assert_eq!(repaired_call_ids(&repairs), vec!["call-2"]);
    }

    #[test]
    fn repair_accepts_a_call_id_reused_after_its_output() {
        // Providers that number calls per request can legitimately repeat an
        // id across turns; each pair is closed before the next opens.
        let items = vec![
            function_call("call-1", "Bash"),
            function_call_output("call-1", "ok"),
            function_call("call-1", "Read"),
            function_call_output("call-1", "contents"),
        ];

        assert!(
            repair_items_for_incomplete_calls(&items)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn repair_rejects_two_unfinished_calls_sharing_a_call_id() {
        let items = vec![
            function_call("call-1", "Bash"),
            function_call("call-1", "Read"),
        ];

        let error = repair_items_for_incomplete_calls(&items).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("two unfinished function calls share call_id 'call-1'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn repair_rejects_an_output_that_precedes_its_call() {
        let items = vec![
            function_call_output("call-1", "ok"),
            function_call("call-1", "Bash"),
        ];

        let error = repair_items_for_incomplete_calls(&items).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("output for call_id 'call-1' has no preceding unmatched function call"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn restored_history_appends_and_queues_repairs() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        state
            .with_restored_history(vec![user_message("go"), function_call("call-1", "Bash")])
            .unwrap();

        assert_eq!(state.history().len(), 4);
        assert!(matches!(
            state.history()[3],
            ConversationItem::FunctionCallOutput { .. }
        ));
        assert_eq!(
            repaired_call_ids(&state.take_pending_repairs()),
            vec!["call-1"]
        );
        assert!(state.take_pending_repairs().is_empty());
    }

    #[test]
    fn restoring_already_repaired_history_is_idempotent() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);
        state
            .with_restored_history(vec![user_message("go"), function_call("call-1", "Bash")])
            .unwrap();
        let repaired = state.history()[1..].to_vec();

        // A later resume reads back the repaired history from the session file.
        let mut resumed = ConversationState::new(&[(Role::System, "sys".to_string())]);
        resumed.with_restored_history(repaired.clone()).unwrap();

        assert!(resumed.take_pending_repairs().is_empty());
        assert_eq!(resumed.history().len(), 1 + repaired.len());
    }

    #[test]
    fn restored_history_rejects_ambiguous_pairing() {
        let mut state = ConversationState::new(&[(Role::System, "sys".to_string())]);

        let error = state
            .with_restored_history(vec![
                function_call("call-1", "Bash"),
                function_call("call-1", "Read"),
            ])
            .unwrap_err();

        assert!(error.to_string().contains("Corrupt session history"));
        // The history is left untouched when restoration fails.
        assert_eq!(state.history().len(), 1);
    }
}
