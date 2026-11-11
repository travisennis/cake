use std::collections::BTreeMap;

use super::*;
use crate::clients::mcp::client::choose_protocol;
use crate::clients::mcp::test_support::{connect_fixture, wait_for_tools_generation};
use crate::config::mcp::{McpExposure, McpServerConfig};

async fn connect(era: &str, timeout_secs: u64) -> (tempfile::TempDir, McpServer) {
    connect_fixture(era, false, McpExposure::Deferred, timeout_secs).await
}

#[tokio::test]
async fn legacy_server_negotiates_and_lists_every_page() {
    let (_directory, server) = connect("legacy", 10).await;

    assert_eq!(server.name, "fake");
    assert_eq!(server.description.as_deref(), Some("Fixture server"));
    assert_eq!(server.exposure, McpExposure::Deferred);
    assert_eq!(server.protocol_version, "2025-11-25");
    assert_eq!(server.server_info.as_deref(), Some("fake-legacy"));
    let names: Vec<&str> = server.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["echo", "get.page", "slow", "fail"]);
    let registered: Vec<&str> = server
        .tools
        .iter()
        .map(|tool| tool.registered_name.as_str())
        .collect();
    assert_eq!(
        registered,
        [
            "mcp__fake__echo",
            "mcp__fake__get_page",
            "mcp__fake__slow",
            "mcp__fake__fail",
        ]
    );
    assert!(
        server.tools[0]
            .description
            .starts_with("MCP server 'fake': Echo")
    );
    assert!(server.tools[0].output_schema.is_some());

    server.shutdown().await;
}

#[tokio::test]
async fn modern_server_negotiates_with_per_request_metadata() {
    let (_directory, server) = connect("modern", 10).await;

    assert_eq!(server.protocol_version, MODERN_PROTOCOL_VERSION);
    assert_eq!(server.server_info.as_deref(), Some("fake-modern"));
    assert_eq!(server.tools.len(), 4);

    server.shutdown().await;
}

#[tokio::test]
async fn tool_call_returns_the_raw_result() {
    let (_directory, server) = connect("legacy", 10).await;

    let result = server
        .client
        .call_tool("echo", serde_json::json!({"message": "hello"}))
        .await
        .unwrap();
    let content = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .unwrap();
    assert_eq!(
        content[0].get("text").and_then(serde_json::Value::as_str),
        Some("echo: hello")
    );
    assert_eq!(
        result
            .get("structuredContent")
            .and_then(|value| value.get("message"))
            .and_then(serde_json::Value::as_str),
        Some("hello")
    );

    server.shutdown().await;
}

#[tokio::test]
async fn tool_error_flag_is_preserved() {
    let (_directory, server) = connect("legacy", 10).await;

    let result = server
        .client
        .call_tool("fail", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(
        result.get("isError").and_then(serde_json::Value::as_bool),
        Some(true)
    );

    server.shutdown().await;
}

#[tokio::test]
async fn slow_tool_reports_a_timeout() {
    let (_directory, server) = connect("legacy", 1).await;

    let error = server
        .client
        .call_tool("slow", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(error.contains("timed out after 1s"), "{error}");

    server.shutdown().await;
}

#[tokio::test]
async fn a_process_that_exits_before_handshake_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let config = McpServerConfig {
        name: "dead".to_string(),
        command: "sh".to_string(),
        args: vec!["-c".to_string(), "echo nope; exit 1".to_string()],
        env: BTreeMap::new(),
        timeout_secs: 5,
        description: None,
        exposure: McpExposure::Direct,
        source: directory.path().join("mcp.json"),
    };

    let error = connect_server(&config, directory.path()).await.unwrap_err();
    assert!(error.contains("closed the connection"), "{error}");
}

#[tokio::test]
async fn legacy_list_change_refreshes_tools() {
    let (_directory, mut server) = connect_fixture("legacy", true, McpExposure::Deferred, 10).await;

    assert_eq!(
        server.tool_list_change_mode(),
        ToolListChangeMode::Notifications
    );
    let initial: Vec<&str> = server.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(initial, ["echo", "get.page", "slow", "fail", "mutate"]);
    assert_eq!(server.tools_generation(), 0);

    server
        .client
        .call_tool("mutate", serde_json::json!({}))
        .await
        .unwrap();
    wait_for_tools_generation(&server, 0).await;

    let changed = server.reload_tools().await.unwrap();
    assert!(changed, "the fixture should report a different tool list");
    let names: Vec<&str> = server.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["echo", "get.page", "slow", "added"]);
    assert!(!server.reload_tools().await.unwrap());

    server.shutdown().await;
}

#[tokio::test]
async fn modern_list_change_subscribes_and_refreshes_tools() {
    let (_directory, mut server) = connect_fixture("modern", true, McpExposure::Deferred, 10).await;

    assert_eq!(
        server.tool_list_change_mode(),
        ToolListChangeMode::Subscription
    );
    assert_eq!(server.tools_generation(), 0);

    server
        .client
        .call_tool("mutate", serde_json::json!({}))
        .await
        .unwrap();
    wait_for_tools_generation(&server, 0).await;

    let changed = server.reload_tools().await.unwrap();
    assert!(changed, "the modern subscription should report the change");
    let names: Vec<&str> = server.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["echo", "get.page", "slow", "added"]);

    server.shutdown().await;
}

#[tokio::test]
async fn a_server_without_a_change_is_not_relisted() {
    let (_directory, mut server) = connect_fixture("legacy", true, McpExposure::Deferred, 10).await;

    assert_eq!(server.tools_generation(), 0);
    assert!(!server.reload_tools().await.unwrap());
    assert_eq!(server.tools_generation(), 0);

    server.shutdown().await;
}

#[test]
fn tool_list_change_mode_labels_are_stable() {
    assert_eq!(ToolListChangeMode::None.as_str(), "none");
    assert_eq!(ToolListChangeMode::Notifications.as_str(), "notifications");
    assert_eq!(ToolListChangeMode::Subscription.as_str(), "subscription");
}

#[test]
fn modern_protocol_selection_prefers_the_known_version() {
    let versions = vec![
        serde_json::json!("2025-11-25"),
        serde_json::json!("2026-07-28"),
    ];
    assert_eq!(
        choose_protocol(Some(&versions)).unwrap().as_deref(),
        Some(MODERN_PROTOCOL_VERSION)
    );
}

#[test]
fn legacy_only_version_list_falls_back() {
    let versions = vec![serde_json::json!("2025-11-25")];
    assert!(choose_protocol(Some(&versions)).unwrap().is_none());
}

#[test]
fn newer_unknown_version_is_an_error() {
    let versions = vec![serde_json::json!("2027-01-01")];
    let error = choose_protocol(Some(&versions)).unwrap_err();
    assert!(error.contains("upgrade cake"), "{error}");
}

#[test]
fn absent_version_list_defaults_to_modern() {
    assert_eq!(
        choose_protocol(None).unwrap().as_deref(),
        Some(MODERN_PROTOCOL_VERSION)
    );
}

#[test]
fn tool_entries_get_registered_names_and_server_prefixed_descriptions() {
    let tool = parse_tool(
        "safari-mcp",
        &serde_json::json!({
            "name": "get.page",
            "description": "Return the page",
            "inputSchema": {"type": "object"},
            "outputSchema": {"type": "array"}
        }),
    )
    .unwrap();
    assert_eq!(tool.name, "get.page");
    assert_eq!(tool.registered_name, "mcp__safari-mcp__get_page");
    assert_eq!(tool.description, "MCP server 'safari-mcp': Return the page");
    assert!(tool.output_schema.is_some());
}

#[test]
fn a_missing_input_schema_becomes_an_empty_object() {
    let tool = parse_tool("server", &serde_json::json!({"name": "ping"})).unwrap();
    assert_eq!(
        tool.input_schema,
        serde_json::json!({"type": "object", "properties": {}})
    );
}

#[test]
fn invalid_tool_entries_are_skipped() {
    assert!(parse_tool("server", &serde_json::json!({"name": "!!"})).is_none());
    assert!(
        parse_tool(
            "server",
            &serde_json::json!({"name": "bad", "inputSchema": "not an object"})
        )
        .is_none()
    );
    assert!(parse_tool("server", &serde_json::json!({"description": "no name"})).is_none());
}
