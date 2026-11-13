use std::collections::VecDeque;
use std::time::Duration;

use super::*;
use crate::config::model::{ApiType, ModelConfig, ReasoningEffort};
use crate::session_telemetry::TerminationClassification;
use crate::types::compaction::{
    CHECKPOINT_VERSION, ContentClass, MessageOrigin, PinReason, RetainedConversationRecord,
    RetainedRecord, SkillRead, SummaryUsage, classify_text, estimate_item_tokens,
    estimate_text_tokens, image_tokens,
};
use crate::types::session::{FunctionCallData, FunctionCallOutputData, MessageData};
use crate::types::{ImagePart, Role, Usage, UsagePresence};

fn record(kind: RetainedConversationRecord) -> RetainedRecord {
    RetainedRecord {
        record: kind,
        pin: None,
        source_task: None,
        origin: None,
        skill_read: None,
    }
}

fn message(role: Role, content: &str) -> RetainedRecord {
    record(RetainedConversationRecord::Message(MessageData {
        role,
        content: content.to_string(),
        id: None,
        status: None,
        timestamp: None,
        images: Vec::new(),
    }))
}

fn user(content: &str) -> RetainedRecord {
    message(Role::User, content)
}

fn assistant(content: &str) -> RetainedRecord {
    message(Role::Assistant, content)
}

fn call(call_id: &str) -> RetainedRecord {
    record(RetainedConversationRecord::FunctionCall(FunctionCallData {
        id: format!("fc-{call_id}"),
        call_id: call_id.to_string(),
        name: "Read".to_string(),
        arguments: "{}".to_string(),
        arguments_parse_error: None,
        replay: None,
        timestamp: None,
    }))
}

fn output(call_id: &str, text: &str) -> RetainedRecord {
    record(RetainedConversationRecord::FunctionCallOutput(
        FunctionCallOutputData {
            call_id: call_id.to_string(),
            output: text.to_string(),
            content_id: None,
            replay: None,
            timestamp: None,
            images: Vec::new(),
        },
    ))
}

fn image_output(call_id: &str, bytes: usize) -> RetainedRecord {
    let mut record = output(call_id, "ReadImage");
    let RetainedConversationRecord::FunctionCallOutput(data) = &mut record.record else {
        unreachable!()
    };
    data.images.push(ImagePart {
        media_type: "image/png".to_string(),
        data_base64: "A".repeat(bytes),
        content_id: None,
    });
    record
}

fn pinned(mut record: RetainedRecord, pin: PinReason) -> RetainedRecord {
    record.pin = Some(pin);
    record
}

fn model() -> ModelIdentity {
    ModelIdentity {
        model: "glm-5.1".to_string(),
        model_config: Some("test".to_string()),
        reasoning_effort: None,
    }
}

fn input<'a>(
    source: &'a [RetainedRecord],
    prefix: usize,
    current_user_input: Option<usize>,
    window: Option<u32>,
    consumed: &'a [usize],
) -> PreparationInput<'a> {
    PreparationInput {
        source,
        prompt_prefix_len: prefix,
        current_user_input,
        current_task_id: "task-1".to_string(),
        fixed: FixedRequest {
            tool_schemas: &[],
            output_schema: None,
        },
        context_window: window,
        measured: None,
        has_active_checkpoint: false,
        consumed_image_indices: consumed,
        session_id: Uuid::nil(),
        task_id: Uuid::nil(),
        model: model(),
        trigger: CompactionTrigger::Threshold,
        policy: CompactionPolicy::default(),
    }
}

fn prepared(outcome: PrepareOutcome) -> Box<PreparedCompaction> {
    match outcome {
        PrepareOutcome::Prepared(prepared) => prepared,
        other => panic!("expected prepared, got {other:?}"),
    }
}

#[test]
fn policy_defaults_and_window_clamps() {
    let default = CompactionPolicy::default().validate(None).unwrap();
    assert!(!default.enabled);
    assert_eq!(default.retained_tokens, 20_000);
    assert_eq!(default.summary_tokens, 4_096);
    assert_eq!(default.trigger_tokens, None);

    let clamped = CompactionPolicy::default().validate(Some(1_000)).unwrap();
    assert_eq!(clamped.retained_tokens, 250);
    assert_eq!(clamped.summary_tokens, 62);
    assert_eq!(clamped.trigger_tokens, Some(800));
    assert_eq!(clamped.target_tokens, Some(600));

    assert!(
        CompactionPolicy {
            threshold_percent: 9,
            ..Default::default()
        }
        .validate(Some(1_000))
        .is_err()
    );
    assert!(
        CompactionPolicy {
            threshold_percent: 81,
            ..Default::default()
        }
        .validate(Some(1_000))
        .is_err()
    );
    assert!(
        CompactionPolicy {
            retained_tokens: 0,
            ..Default::default()
        }
        .validate(None)
        .is_err()
    );
    assert!(
        CompactionPolicy {
            summary_tokens: 0,
            ..Default::default()
        }
        .validate(None)
        .is_err()
    );
    assert!(CompactionPolicy::default().validate(Some(8)).is_err());
}

#[test]
fn estimator_is_deterministic_and_content_classed() {
    assert_eq!(classify_text("plain prose"), ContentClass::Prose);
    assert_eq!(classify_text(r#"{"a":1}"#), ContentClass::Code);
    assert_eq!(classify_text(&"A".repeat(256)), ContentClass::Opaque);

    // 4 prose characters -> 1 raw token -> ceil(1.25) = 2.
    assert_eq!(estimate_text_tokens("abcd"), 2);
    // 7 JSON characters -> ceil(7/3) = 3 raw -> ceil(3.75) = 4.
    assert_eq!(estimate_text_tokens(r#"{"a":1}"#), 4);
    // 256 opaque characters -> 128 raw -> 160 after the margin.
    assert_eq!(estimate_text_tokens(&"A".repeat(256)), 160);

    let image = image_output("call-1", 5 * 1024 * 1024);
    let RetainedConversationRecord::FunctionCallOutput(data) = &image.record else {
        unreachable!()
    };
    let record = RetainedRecord {
        record: RetainedConversationRecord::FunctionCallOutput(data.clone()),
        pin: None,
        source_task: None,
        origin: None,
        skill_read: None,
    };
    let item = record.to_conversation_item();
    // The 5 MiB base64 payload contributes the flat image cost, not its bytes.
    assert!(estimate_item_tokens(&item) < 2_000);
    assert!(estimate_item_tokens(&item) >= image_tokens(1));
}

#[test]
fn prepare_no_op_for_empty_projection() {
    let source = vec![message(Role::System, "system")];
    let outcome = prepare(input(&source, 1, None, None, &[])).unwrap();
    assert!(matches!(outcome, PrepareOutcome::NoOp { .. }));
}

#[test]
fn prepare_retires_whole_exchanges_to_the_window_target() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    for index in 0..5 {
        source.push(call(&format!("call-{index}")));
        source.push(output(&format!("call-{index}"), &"x".repeat(4_000)));
        source.push(assistant(&format!("answer {index}")));
    }

    let prepared = prepared(prepare(input(&source, 1, Some(1), Some(10_000), &[])).unwrap());
    assert!(prepared.estimated_input.after_tokens <= 6_000);
    let reduction = prepared
        .estimated_input
        .before_tokens
        .saturating_sub(prepared.estimated_input.after_tokens);
    assert!(reduction >= prepared.estimated_input.before_tokens.saturating_add(9) / 10);
    assert!(!prepared.retired_exchanges.is_empty());
    let retired: usize = prepared.retired_exchanges.iter().map(Vec::len).sum();
    assert_eq!(retired + prepared.retained.len(), source.len() - 1);
    // Every call kept its output, and the whole newest exchange is retained.
    for exchange in &prepared.retired_exchanges {
        let has_call = exchange
            .iter()
            .any(|record| matches!(record.record, RetainedConversationRecord::FunctionCall(_)));
        let has_output = exchange.iter().any(|record| {
            matches!(
                record.record,
                RetainedConversationRecord::FunctionCallOutput(_)
            )
        });
        assert_eq!(has_call, has_output);
    }
    assert!(
        prepared
            .retained
            .iter()
            .any(|record| record.pin == Some(PinReason::UserInput))
    );
}

#[test]
fn prepare_rejects_a_200_token_reduction_against_an_80k_trigger() {
    // 80,100 -> 79,900 tokens: after still exceeds the 60,000 target derived
    // from an 80,000 trigger, so the candidate must be no-fit.
    let huge = "x".repeat(160_000);
    let mut first = pinned(user(&huge), PinReason::UserInput);
    first.origin = Some(MessageOrigin::User);
    let mut second = pinned(user(&huge), PinReason::UserInput);
    second.origin = Some(MessageOrigin::User);
    let mut source = vec![message(Role::System, "system"), first, second];
    source.push(assistant(&"y".repeat(800)));

    let outcome = prepare(input(&source, 1, Some(2), Some(100_000), &[])).unwrap();
    let reason = outcome
        .no_fit_reason()
        .expect("the candidate must not fit the 60% target");
    assert!(
        reason.contains("cannot reach the compaction bounds"),
        "{reason}"
    );
}

#[test]
fn pins_keep_current_input_skills_and_unconsumed_images() {
    let mut skill_output = output("skill-call", "skill instructions");
    skill_output.skill_read = Some(SkillRead {
        name: "debugging-cake".to_string(),
        path: "/skills/debugging-cake/SKILL.md".into(),
    });
    skill_output.source_task = Some("task-1".to_string());
    let mut historical = output("old-call", "old skill");
    historical.skill_read = Some(SkillRead {
        name: "old".to_string(),
        path: "/skills/old/SKILL.md".into(),
    });
    historical.source_task = Some("task-0".to_string());

    let source = vec![
        message(Role::System, "system"),
        user("current request"),
        call("skill-call"),
        skill_output,
        call("old-call"),
        historical,
        pinned(image_output("image-call", 64), PinReason::UnconsumedImage),
        image_output("consumed-image", 64),
    ];
    let consumed = [7_usize];
    let pinned_source = apply_pins(&input(&source, 1, Some(1), Some(10_000), &consumed)).unwrap();

    assert_eq!(pinned_source[1].pin, Some(PinReason::UserInput));
    assert!(matches!(
        pinned_source[3].pin,
        Some(PinReason::SkillInstructions { .. })
    ));
    assert_eq!(pinned_source[5].pin, None);
    assert_eq!(pinned_source[6].pin, Some(PinReason::UnconsumedImage));
    assert_eq!(pinned_source[7].pin, None);

    let missing = apply_pins(&input(&source, 1, Some(1), Some(10_000), &[]))
        .expect_err("unconsumed images need evidence");
    assert!(missing.to_string().contains("consumption evidence"));
}

#[test]
fn prepare_keeps_large_payloads_out_of_the_checkpoint_ceiling() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    source.push(call("image-1"));
    source.push(pinned(
        image_output("image-1", 5 * 1024 * 1024),
        PinReason::UnconsumedImage,
    ));
    source.push(call("image-2"));
    source.push(pinned(
        image_output("image-2", 5 * 1024 * 1024),
        PinReason::UnconsumedImage,
    ));
    for index in 0..6 {
        source.push(call(&format!("call-{index}")));
        source.push(output(&format!("call-{index}"), &"z".repeat(40_000)));
        source.push(assistant(&format!("answer {index}")));
    }

    let prepared = prepared(prepare(input(&source, 1, Some(1), Some(100_000), &[])).unwrap());
    let checkpoint = candidate_checkpoint(
        &input(&source, 1, Some(1), Some(100_000), &[]),
        "checkpoint-1",
        &prepared.retained,
        prepared.estimated_input,
    );
    assert!(checkpoint_bytes_with_reservation(&checkpoint).unwrap() < MAX_CHECKPOINT_RECORD_BYTES);
}

#[tokio::test]
async fn summarize_single_request_records_usage_and_validates_the_checkpoint() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    for index in 0..4 {
        source.push(call(&format!("call-{index}")));
        source.push(output(&format!("call-{index}"), &"q".repeat(4_000)));
        source.push(assistant(&format!("answer {index}")));
    }
    let prepared = prepared(prepare(input(&source, 1, Some(1), Some(10_000), &[])).unwrap());
    let mut caller = FakeCaller::new(vec![FakeResponse::ok("A concise checkpoint summary.")]);

    let checkpoint = summarize(*prepared, &mut caller).await.unwrap();
    assert_eq!(caller.calls.len(), 1);
    assert_eq!(checkpoint.summary, "A concise checkpoint summary.");
    assert_eq!(checkpoint.summary_usage.unwrap().input_tokens, 100);
    assert!(checkpoint.summary_time_ms.is_some());
    assert_eq!(checkpoint.checkpoint_version, CHECKPOINT_VERSION);
    assert!(checkpoint.to_record_bytes().unwrap().len() <= MAX_CHECKPOINT_RECORD_BYTES);
}

#[tokio::test]
async fn summarize_batches_oldest_exchanges_and_carries_the_summary() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    for index in 0..3 {
        source.push(call(&format!("call-{index}")));
        source.push(output(&format!("call-{index}"), &"b".repeat(500_000)));
        source.push(assistant(&format!("answer {index}")));
    }
    let prepared = prepared(prepare(input(&source, 1, Some(1), Some(100_000), &[])).unwrap());
    assert!(prepared.retired_exchanges.len() > 1);
    let mut caller = FakeCaller::new(vec![
        FakeResponse::ok("first summary"),
        FakeResponse::ok("second summary"),
        FakeResponse::ok("third summary"),
    ]);

    let checkpoint = summarize(*prepared, &mut caller).await.unwrap();
    assert!(caller.calls.len() > 1);
    assert!(caller.calls[1].1.contains("[summary so far]"));
    assert_eq!(checkpoint.summary, "third summary");
}

#[tokio::test]
async fn summarize_rejects_tool_calls_refusals_empty_text_and_over_budget() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    for index in 0..3 {
        source.push(call(&format!("call-{index}")));
        source.push(output(&format!("call-{index}"), &"r".repeat(4_000)));
        source.push(assistant(&format!("answer {index}")));
    }

    let prepared = || prepared(prepare(input(&source, 1, Some(1), Some(10_000), &[])).unwrap());

    let mut tool_caller = FakeCaller::new(vec![FakeResponse::tool_call()]);
    assert!(summarize(*prepared(), &mut tool_caller).await.is_err());

    let mut refusal = FakeCaller::new(vec![FakeResponse::termination(
        TerminationClassification::Failed,
    )]);
    assert!(summarize(*prepared(), &mut refusal).await.is_err());

    let mut empty = FakeCaller::new(vec![FakeResponse::ok("   ")]);
    assert!(summarize(*prepared(), &mut empty).await.is_err());

    let mut oversized = FakeCaller::new(vec![FakeResponse::ok(&"w".repeat(80_000))]);
    assert!(summarize(*prepared(), &mut oversized).await.is_err());
}

#[tokio::test]
async fn summarize_checks_backend_support_before_any_call() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    source.push(call("call-1"));
    source.push(output("call-1", &"s".repeat(4_000)));
    source.push(assistant("answer"));
    let prepared = || prepared(prepare(input(&source, 1, Some(1), Some(10_000), &[])).unwrap());

    let mut unsupported = FakeCaller::new(vec![FakeResponse::ok("summary")]);
    unsupported.supports = false;
    assert!(summarize(*prepared(), &mut unsupported).await.is_err());
    assert!(unsupported.calls.is_empty());

    let mut invalid = FakeCaller::new(vec![FakeResponse::ok("summary")]);
    invalid.valid = false;
    assert!(summarize(*prepared(), &mut invalid).await.is_err());
    assert!(invalid.calls.is_empty());
}

#[test]
fn summary_config_changes_only_the_generation_cap() {
    let mut base = config("https://example.com/v1", ApiType::ChatCompletions);
    base.model_config.reasoning_effort = Some(ReasoningEffort::High);
    base.model_config.reasoning_max_tokens = Some(2_000);

    let summary = summary_config(&base, 8_000);

    assert_eq!(summary.model_config.model, base.model_config.model);
    assert_eq!(summary.model_config.api_type, base.model_config.api_type);
    assert_eq!(summary.model_config.base_url, base.model_config.base_url);
    assert_eq!(
        summary.model_config.api_key_env,
        base.model_config.api_key_env
    );
    assert_eq!(
        summary.model_config.reasoning_effort,
        base.model_config.reasoning_effort
    );
    assert_eq!(
        summary.model_config.reasoning_max_tokens,
        base.model_config.reasoning_max_tokens
    );
    assert_eq!(summary.model_config.max_output_tokens, Some(8_000));
    assert_eq!(base.model_config.max_output_tokens, None);
    assert_eq!(summary.api_key, base.api_key);
}

#[test]
fn summarize_rejects_more_than_four_logical_requests() {
    let prepared = PreparedCompaction {
        checkpoint_id: "checkpoint-1".to_string(),
        retained: Vec::new(),
        retired_exchanges: (0..5)
            .map(|index| vec![assistant(&format!("{index} {}", "word ".repeat(75_000)))])
            .collect(),
        estimated_input: crate::types::compaction::EstimatedInput {
            before_tokens: 600_000,
            after_tokens: 1_000,
            basis: crate::types::compaction::EstimateBasis::ContentClass,
        },
        summary_tokens: 1_000,
        context_window: Some(100_000),
        session_id: Uuid::nil(),
        task_id: Uuid::nil(),
        model: model(),
        trigger: CompactionTrigger::Threshold,
    };
    let limits = PromptLimits::from_prepared(&prepared);
    let error = pack_batches(&prepared, limits).unwrap_err();
    assert!(
        error.to_string().contains("more than 4 logical requests"),
        "{error}"
    );
}

#[tokio::test]
async fn summarize_rejects_a_failed_batch() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    for index in 0..3 {
        source.push(call(&format!("call-{index}")));
        source.push(output(&format!("call-{index}"), &"d".repeat(500_000)));
        source.push(assistant(&format!("answer {index}")));
    }
    let prepared = prepared(prepare(input(&source, 1, Some(1), Some(100_000), &[])).unwrap());
    let mut caller = FakeCaller::new(vec![FakeResponse::ok("first"), FakeResponse::failure()]);

    assert!(summarize(*prepared, &mut caller).await.is_err());
    assert!(caller.calls.len() > 1);
}

#[test]
fn measured_basis_requires_complete_matching_fresh_usage() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    for index in 0..5 {
        source.push(call(&format!("call-{index}")));
        source.push(output(&format!("call-{index}"), &"m".repeat(20_000)));
        source.push(assistant(&format!("answer {index}")));
    }

    let build = |measured: Option<MeasuredInput>, active: bool| PreparationInput {
        source: &source,
        prompt_prefix_len: 1,
        current_user_input: Some(1),
        current_task_id: "task-1".to_string(),
        fixed: FixedRequest {
            tool_schemas: &[],
            output_schema: None,
        },
        context_window: Some(100_000),
        measured,
        has_active_checkpoint: active,
        consumed_image_indices: &[],
        session_id: Uuid::nil(),
        task_id: Uuid::nil(),
        model: model(),
        trigger: CompactionTrigger::Threshold,
        policy: CompactionPolicy::default(),
    };

    let measured = |presence, model: &str, covered: usize, input_tokens: u64| MeasuredInput {
        input_tokens,
        covered_item_count: covered,
        model: model.to_string(),
        presence,
    };

    let candidate = prepared(
        prepare(build(
            Some(measured(UsagePresence::Complete, "glm-5.1", 2, 50_000)),
            false,
        ))
        .unwrap(),
    );
    assert_eq!(
        candidate.estimated_input.basis,
        EstimateBasis::MeasuredUsage
    );
    assert!(candidate.estimated_input.before_tokens >= 50_000);

    for (measured, active) in [
        (
            Some(measured(UsagePresence::Partial, "glm-5.1", 2, 50_000)),
            false,
        ),
        (
            Some(measured(UsagePresence::Complete, "other-model", 2, 50_000)),
            false,
        ),
        (
            Some(measured(UsagePresence::Complete, "glm-5.1", 9999, 50_000)),
            false,
        ),
        (
            Some(measured(UsagePresence::Complete, "glm-5.1", 2, 0)),
            false,
        ),
        (
            Some(measured(UsagePresence::Complete, "glm-5.1", 2, 50_000)),
            true,
        ),
    ] {
        let candidate = prepared(prepare(build(measured, active)).unwrap());
        assert_eq!(candidate.estimated_input.basis, EstimateBasis::ContentClass);
    }
}

#[test]
fn fixed_request_tokens_count_tool_and_output_schema_bytes() {
    let base = fixed_request_tokens(&FixedRequest {
        tool_schemas: &[],
        output_schema: None,
    });
    let tool = Tool {
        type_: "function".to_string(),
        name: "Bash".to_string(),
        description: "x".repeat(4_000),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    };
    let with_tool = fixed_request_tokens(&FixedRequest {
        tool_schemas: &[tool],
        output_schema: None,
    });
    assert!(with_tool > base + 500);

    let schema = serde_json::json!({
        "type": "object",
        "properties": {"answer": {"type": "string", "description": "y".repeat(4_000)}}
    });
    let with_schema = fixed_request_tokens(&FixedRequest {
        tool_schemas: &[],
        output_schema: Some(&schema),
    });
    assert!(with_schema > base + 500);
}

#[test]
fn ambiguous_provenance_and_missing_payload_references_are_rejected() {
    let mut ambiguous = output("skill", "instructions");
    ambiguous.skill_read = Some(SkillRead {
        name: "debugging-cake".to_string(),
        path: "/skills/debugging-cake/SKILL.md".into(),
    });
    let source = vec![message(Role::System, "system"), user("request"), ambiguous];
    let error = apply_pins(&input(&source, 1, Some(1), Some(10_000), &[])).unwrap_err();
    assert!(
        error.to_string().contains("ambiguous legacy provenance"),
        "{error}"
    );
    let outcome = prepare(input(&source, 1, Some(1), Some(10_000), &[])).unwrap();
    assert!(outcome.no_fit_reason().is_some());

    let mut broken = output("image", "ReadImage");
    let RetainedConversationRecord::FunctionCallOutput(data) = &mut broken.record else {
        unreachable!()
    };
    data.images.push(ImagePart {
        media_type: "image/png".to_string(),
        data_base64: String::new(),
        content_id: None,
    });
    let source = vec![message(Role::System, "system"), user("request"), broken];
    let error = apply_pins(&input(&source, 1, Some(1), Some(10_000), &[])).unwrap_err();
    assert!(
        error.to_string().contains("neither inline bytes"),
        "{error}"
    );
}

#[test]
fn repeated_skill_reads_are_pinned_independently() {
    let mut first = output("skill-1", "first read");
    first.skill_read = Some(SkillRead {
        name: "debugging-cake".to_string(),
        path: "/skills/debugging-cake/SKILL.md".into(),
    });
    first.source_task = Some("task-1".to_string());
    let mut second = output("skill-2", "second read");
    second.skill_read = first.skill_read.clone();
    second.source_task = Some("task-1".to_string());

    let source = vec![
        message(Role::System, "system"),
        user("request"),
        call("skill-1"),
        first,
        call("skill-2"),
        second,
    ];
    let pinned_source = apply_pins(&input(&source, 1, Some(1), Some(10_000), &[])).unwrap();
    assert!(matches!(
        pinned_source[3].pin,
        Some(PinReason::SkillInstructions { .. })
    ));
    assert!(matches!(
        pinned_source[5].pin,
        Some(PinReason::SkillInstructions { .. })
    ));
}

#[test]
fn pins_that_exceed_the_target_are_no_fit() {
    let mut source = vec![message(Role::System, "system"), user(&"p".repeat(100_000))];
    source.push(assistant("small retirable answer"));

    let outcome = prepare(input(&source, 1, Some(1), Some(10_000), &[])).unwrap();
    assert!(outcome.no_fit_reason().is_some());
}

#[test]
fn legacy_inline_images_externalize_before_the_checkpoint_ceiling() {
    let mut image_message = message(Role::User, "look");
    let RetainedConversationRecord::Message(data) = &mut image_message.record else {
        unreachable!()
    };
    data.images.push(ImagePart {
        media_type: "image/png".to_string(),
        data_base64: "A".repeat(5 * 1024 * 1024),
        content_id: None,
    });
    let checkpoint = crate::types::compaction::test_support::checkpoint(
        "checkpoint-1",
        "summary",
        vec![image_message],
    );

    let dir = tempfile::tempdir().unwrap();
    let store = crate::config::BlobStore::new(dir.path().join("blobs"));
    let externalized = store.externalize_checkpoint(&checkpoint).unwrap();
    assert!(externalized.to_record_bytes().unwrap().len() <= MAX_CHECKPOINT_RECORD_BYTES);
    let RetainedConversationRecord::Message(data) = &externalized.retained[0].record else {
        unreachable!()
    };
    assert!(data.images[0].content_id.is_some());
    assert!(data.images[0].data_base64.is_empty());
}

#[test]
fn byte_pressure_retires_inline_outputs_below_the_externalization_threshold() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    for index in 0..560 {
        source.push(call(&format!("call-{index}")));
        source.push(output(&format!("call-{index}"), &"z".repeat(15_000)));
        source.push(assistant(&format!("answer {index}")));
    }

    let prepared = prepared(prepare(input(&source, 1, Some(1), Some(10_000_000), &[])).unwrap());
    assert!(!prepared.retired_exchanges.is_empty());
    let checkpoint = candidate_checkpoint(
        &input(&source, 1, Some(1), Some(10_000_000), &[]),
        "checkpoint-1",
        &prepared.retained,
        prepared.estimated_input,
    );
    assert!(checkpoint_bytes_with_reservation(&checkpoint).unwrap() <= MAX_CHECKPOINT_RECORD_BYTES);
}

#[test]
fn pinned_inline_outputs_over_the_byte_ceiling_are_no_fit() {
    let mut source = vec![message(Role::System, "system"), user("current request")];
    for index in 0..560 {
        source.push(call(&format!("call-{index}")));
        let mut output = output(&format!("call-{index}"), &"z".repeat(15_000));
        output.skill_read = Some(SkillRead {
            name: format!("skill-{index}"),
            path: format!("/skills/skill-{index}/SKILL.md").into(),
        });
        output.source_task = Some("task-1".to_string());
        source.push(output);
        source.push(assistant(&format!("answer {index}")));
    }

    let outcome = prepare(input(&source, 1, Some(1), Some(10_000_000), &[])).unwrap();
    assert!(outcome.no_fit_reason().is_some());
}

#[test]
fn codex_support_check_fails_without_a_request() {
    let codex = config("https://chatgpt.com/backend-api/codex", ApiType::Responses);
    assert!(!backend_supports_generation_cap(Backend::Responses, &codex));
    let responses = config("https://api.openai.com/v1", ApiType::Responses);
    assert!(backend_supports_generation_cap(
        Backend::Responses,
        &responses
    ));
    let chat = config("https://example.com/v1", ApiType::ChatCompletions);
    assert!(backend_supports_generation_cap(
        Backend::ChatCompletions,
        &chat
    ));
    assert_eq!(summary_generation_cap(&codex), 16_384);
}

#[test]
fn summary_usage_adds_each_attempt_once() {
    let mut total = SummaryUsage::default();
    add_summary_usage(
        &mut total,
        Usage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            output_tokens_details: crate::types::OutputTokensDetails {
                reasoning_tokens: 2,
            },
            ..Usage::default()
        },
    );
    add_summary_usage(
        &mut total,
        Usage {
            input_tokens: 20,
            output_tokens: 8,
            total_tokens: 28,
            ..Usage::default()
        },
    );
    assert_eq!(total.input_tokens, 30);
    assert_eq!(total.output_tokens, 13);
    assert_eq!(total.reasoning_tokens, 2);
    assert_eq!(total.summary_visible_tokens, 11);
    assert_eq!(total.total_tokens, 43);
}

fn config(base_url: &str, api_type: ApiType) -> ResolvedModelConfig {
    let model_config: ModelConfig = serde_json::from_value(serde_json::json!({
        "model": "test-model",
        "api_type": match api_type {
            ApiType::Responses => "responses",
            ApiType::ChatCompletions => "chat_completions",
        },
        "base_url": base_url,
        "api_key_env": "TEST_KEY",
        "providers": [],
        "supports_images": false
    }))
    .unwrap();
    ResolvedModelConfig {
        model_config,
        api_key: "test".to_string(),
    }
}

#[derive(Clone)]
struct FakeResponse {
    text: Option<String>,
    tool_call: bool,
    termination: TerminationClassification,
    attempts: Vec<ReportedUsage>,
    fail: bool,
}

impl FakeResponse {
    fn ok(text: &str) -> Self {
        Self {
            text: Some(text.to_string()),
            tool_call: false,
            termination: TerminationClassification::Completed,
            attempts: vec![ReportedUsage::new(
                Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    total_tokens: 120,
                    ..Usage::default()
                },
                UsagePresence::Complete,
            )],
            fail: false,
        }
    }

    fn tool_call() -> Self {
        Self {
            text: Some("call a tool".to_string()),
            tool_call: true,
            termination: TerminationClassification::Completed,
            attempts: Vec::new(),
            fail: false,
        }
    }

    fn termination(termination: TerminationClassification) -> Self {
        Self {
            text: Some("refused".to_string()),
            tool_call: false,
            termination,
            attempts: Vec::new(),
            fail: false,
        }
    }

    fn failure() -> Self {
        Self {
            text: None,
            tool_call: false,
            termination: TerminationClassification::Unknown,
            attempts: Vec::new(),
            fail: true,
        }
    }
}

struct FakeCaller {
    responses: VecDeque<FakeResponse>,
    calls: Vec<(u32, String)>,
    supports: bool,
    valid: bool,
}

impl FakeCaller {
    fn new(responses: Vec<FakeResponse>) -> Self {
        Self {
            responses: responses.into(),
            calls: Vec::new(),
            supports: true,
            valid: true,
        }
    }
}

impl SummaryCaller for FakeCaller {
    fn supports_generation_cap(&self) -> bool {
        self.supports
    }

    fn configured_output_max(&self) -> Option<u32> {
        Some(8_000)
    }

    fn validate_summary_request(&self, _generation_cap: u32) -> anyhow::Result<()> {
        anyhow::ensure!(self.valid, "invalid summary request");
        Ok(())
    }

    fn call(
        &mut self,
        request: SummaryCallRequest<'_>,
    ) -> impl std::future::Future<Output = anyhow::Result<SummaryTurn>> + Send {
        std::future::ready(self.respond(&request))
    }
}

impl FakeCaller {
    fn respond(&mut self, request: &SummaryCallRequest<'_>) -> anyhow::Result<SummaryTurn> {
        let prompt = request
            .history
            .iter()
            .filter_map(|item| match item {
                ConversationItem::Message { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.calls.push((request.step, prompt));
        let response = self
            .responses
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("unexpected summary call"))?;
        anyhow::ensure!(!response.fail, "summary provider failed");
        let mut items = Vec::new();
        if let Some(text) = response.text {
            items.push(ConversationItem::Message {
                role: Role::Assistant,
                content: text,
                id: None,
                status: Some("completed".to_string()),
                timestamp: None,
                images: Vec::new(),
            });
        }
        if response.tool_call {
            items.push(ConversationItem::FunctionCall {
                id: "fc-1".to_string(),
                call_id: "call-1".to_string(),
                name: "Bash".to_string(),
                arguments: "{}".to_string(),
                timestamp: None,
            });
        }
        Ok(SummaryTurn {
            items,
            termination: Some(ProviderTermination {
                classification: response.termination,
                provider_status: None,
                provider_reason: None,
            }),
            usage: response.attempts.first().copied(),
            attempts: response.attempts,
            elapsed: Duration::from_millis(10),
        })
    }
}
