use super::*;

#[test]
fn parses_add_with_env_args_and_timeout() {
    let parsed = McpCommand::try_parse_from([
        "cake mcp",
        "add",
        "fixture",
        "--env",
        "FAKE_MCP_ERA=modern",
        "--env",
        "TOKEN=",
        "--timeout-secs",
        "30",
        "--",
        "python3",
        "server.py",
        "--flag",
    ])
    .unwrap();

    let McpSubcommand::Add(args) = parsed.command else {
        panic!("expected mcp add");
    };
    assert_eq!(args.name, "fixture");
    assert_eq!(
        args.env,
        vec![
            ("FAKE_MCP_ERA".to_string(), "modern".to_string()),
            ("TOKEN".to_string(), String::new()),
        ]
    );
    assert_eq!(args.timeout_secs, Some(30));
    assert_eq!(args.command, ["python3", "server.py", "--flag"]);
}

#[test]
fn add_requires_a_transport() {
    let error = McpCommand::try_parse_from(["cake mcp", "add", "fixture"]).unwrap_err();
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
}

#[test]
fn add_rejects_mixed_transports() {
    let error = McpCommand::try_parse_from([
        "cake mcp",
        "add",
        "fixture",
        "--url",
        "https://example.com/mcp",
        "--",
        "python3",
    ])
    .unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn add_requires_trailing_command_or_url() {
    let parsed =
        McpCommand::try_parse_from(["cake mcp", "add", "fixture", "--url", "https://x"]).unwrap();
    let McpSubcommand::Add(args) = parsed.command else {
        panic!("expected mcp add");
    };
    let message = add_command(&args).unwrap_err().to_string();
    assert!(message.contains("stdio servers only"), "{message}");
}

#[test]
fn parses_list_get_and_remove() {
    let parsed = McpCommand::try_parse_from(["cake mcp", "list", "--json"]).unwrap();
    assert!(matches!(parsed.command, McpSubcommand::List { json: true }));

    let parsed = McpCommand::try_parse_from(["cake mcp", "get", "fixture", "--json"]).unwrap();
    let McpSubcommand::Get { name, json } = parsed.command else {
        panic!("expected mcp get");
    };
    assert_eq!(name, "fixture");
    assert!(json);

    let parsed = McpCommand::try_parse_from(["cake mcp", "remove", "fixture"]).unwrap();
    let McpSubcommand::Remove { name } = parsed.command else {
        panic!("expected mcp remove");
    };
    assert_eq!(name, "fixture");
}

#[test]
fn env_pairs_require_a_name_and_a_valid_key() {
    assert_eq!(
        parse_env_pair("TOKEN=secret").unwrap(),
        ("TOKEN".to_string(), "secret".to_string())
    );
    assert_eq!(
        parse_env_pair("EMPTY=").unwrap(),
        ("EMPTY".to_string(), String::new())
    );
    for invalid in ["TOKEN", "=value", "1TOKEN=value", "BAD-NAME=value"] {
        assert!(parse_env_pair(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn text_output_names_environment_variables_without_values() {
    let entry = McpEntry {
        name: "fixture".to_string(),
        command: "python3".to_string(),
        args: vec!["server.py".to_string()],
        env: vec!["FAKE_MCP_ERA".to_string()],
        timeout_secs: 30,
        source: "/tmp/mcp.json".to_string(),
    };
    let mut output = String::new();
    format_entry(&entry, &mut output);

    assert!(output.contains("command: python3 server.py"), "{output}");
    assert!(output.contains("env: FAKE_MCP_ERA"), "{output}");
    assert!(output.contains("timeout: 30s"), "{output}");
    assert!(!output.contains("modern"), "{output}");
}
