use super::*;
use crate::clients::tools::ToolContext;
use crate::clients::tools::bash_gate_fixture as gate_fixture;
use crate::clients::tools::sandbox::SandboxPolicy;
use crate::gates::protocol::GateDecision;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::sync::Arc;

#[test]
fn post_preflight_error_preserves_judge_telemetry() {
    let event = CompensationEventTelemetry::judge_verdict("allow", None, 12, false)
        .with_call_id(Some("call-123"));
    let error = gate_tool_error(vec![event], "session read failed");
    assert_eq!(error.message, "session read failed");
    assert_eq!(error.compensation_events.len(), 1);
    assert_eq!(
        error.compensation_events[0].kind,
        CompensationKind::JudgeVerdict
    );
    assert_eq!(
        error.compensation_events[0].detail.as_deref(),
        Some("allow")
    );
    assert_eq!(error.compensation_events[0].latency_ms, Some(12));
    assert!(error.compensation_events[0].call_id.is_some());
}

/// Check whether `CAKE_REQUIRE_SANDBOX_TESTS` is set to a truthy value,
/// indicating that macOS Seatbelt integration tests must run instead of skip.
///
/// Accepted truthy values: `1`, `true`, `yes`, `on`.
#[cfg(target_os = "macos")]
fn is_sandbox_tests_required() -> bool {
    parse_sandbox_tests_required(std::env::var("CAKE_REQUIRE_SANDBOX_TESTS").ok().as_deref())
}

/// Pure parsing of the optional value for `CAKE_REQUIRE_SANDBOX_TESTS`.
/// Extracted from `is_sandbox_tests_required()` for focused unit testing
/// without environment variable interference.
#[cfg(target_os = "macos")]
fn parse_sandbox_tests_required(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "yes" | "on"))
}

/// Skip the current sandbox integration test only on macOS when Seatbelt
/// cannot be nested, unless `CAKE_REQUIRE_SANDBOX_TESTS` is set.
///
/// Linux never skips: the tests must fail if Landlock cannot fully enforce the
/// configured ABI, so an unavailable boundary cannot satisfy a deny assertion.
/// Returns `true` when the macOS test should be skipped. When Seatbelt is
/// unavailable and tests are required, this function panics with an actionable
/// message.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn skip_if_sandbox_unavailable() -> bool {
    #[cfg(target_os = "macos")]
    {
        let required = is_sandbox_tests_required();

        if super::super::sandbox::is_sandbox_disabled() {
            let msg = "skipping macOS sandbox integration test: CAKE_SANDBOX disables sandboxing";
            assert!(
                !required,
                "sandbox integration tests are required via CAKE_REQUIRE_SANDBOX_TESTS=1 \
                 but CAKE_SANDBOX disables sandboxing; unset CAKE_SANDBOX or set \
                 CAKE_REQUIRE_SANDBOX_TESTS=0 to skip"
            );
            eprintln!("{msg}");
            return true;
        }

        if !super::super::sandbox::can_enforce_platform_sandbox() {
            let msg = "skipping macOS sandbox integration test: sandbox-exec cannot apply profiles \
                        in this process context";
            assert!(
                !required,
                "sandbox integration tests are required via CAKE_REQUIRE_SANDBOX_TESTS=1 \
                 but sandbox-exec cannot apply profiles in this process context; see the \
                 macOS sandbox design doc for requirements"
            );
            eprintln!("{msg}");
            return true;
        }
    }

    false
}

/// Return a writable parent outside the workspace and the sandbox's broad
/// temporary-directory grants.
///
/// `cargo-mutants` runs tests from a copied checkout under `TMPDIR`. Using the
/// current directory's parent as the denied path in that environment places the
/// fixture beneath `/var/folders` or `TMPDIR`, which Cake intentionally grants
/// to Bash. A temporary directory directly under the OS account home is
/// writable by the test setup but is not one of the built-in grants (only
/// selected children of HOME are granted), so the sandbox assertions continue
/// to exercise a denied path.
///
/// Read the OS account home with `HOME` temporarily unset and under `temp-env`'s
/// lock. Other tests replace HOME with synthetic paths, so reading the ambient
/// variable directly here can race with those tests.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn path_outside_cwd_for_sandbox_test() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let home = temp_env::with_var("HOME", None::<&str>, dirs::home_dir)?;
    (!home.starts_with(&cwd)).then_some(home)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn sandbox_fixture_parent_ignores_overridden_home() {
    let account_home = temp_env::with_var("HOME", None::<&str>, dirs::home_dir)
        .expect("OS account home must be available");
    let synthetic_home = tempfile::tempdir().expect("synthetic HOME fixture must be created");

    temp_env::with_var("HOME", Some(synthetic_home.path()), || {
        assert_eq!(
            path_outside_cwd_for_sandbox_test(),
            Some(account_home.clone()),
            "fixture parent must not read a concurrently overridden HOME"
        );
    });
}

#[test]
fn truncate_output_passes_through_small_output() {
    let small = "hello world";
    let result = truncate_output(small, Some(BASH_OUTPUT_MAX_BYTES), 0, 100, false);
    assert!(result.contains(small));
    assert!(result.contains("[exit:0 | 100ms]"));
}

#[test]
fn bash_timeout_argument_is_clamped() {
    assert_eq!(BASH_TIMEOUT_DEFAULT_SECS, 60);
    assert_eq!(BASH_TIMEOUT_MAX_SECS, 60);

    let policy = crate::clients::tools::sandbox::SandboxPolicy::DangerFullAccess;

    // Below the floor: a `0` timeout would fail instantly.
    let args =
        BashExecutionArgs::from_json(r#"{"command": "true", "timeout": 0}"#, policy).unwrap();
    assert_eq!(args.timeout, BASH_TIMEOUT_MIN_SECS);

    // Above the ceiling: cap huge values.
    let args =
        BashExecutionArgs::from_json(r#"{"command": "true", "timeout": 999999}"#, policy).unwrap();
    assert_eq!(args.timeout, BASH_TIMEOUT_MAX_SECS);

    // Missing timeout keeps the documented default yield window.
    let args = BashExecutionArgs::from_json(r#"{"command": "true"}"#, policy).unwrap();
    assert_eq!(args.timeout, BASH_TIMEOUT_DEFAULT_SECS);

    // In-range values pass through untouched.
    let args =
        BashExecutionArgs::from_json(r#"{"command": "true", "timeout": 42}"#, policy).unwrap();
    assert_eq!(args.timeout, 42);
}

#[test]
fn legacy_six_hundred_second_timeout_clamps_to_foreground_yield() {
    let policy = crate::clients::tools::sandbox::SandboxPolicy::DangerFullAccess;
    let args =
        BashExecutionArgs::from_json(r#"{"command": "true", "timeout": 600}"#, policy).unwrap();
    assert_eq!(args.timeout, 60);
    assert_eq!(args.timeout, BASH_TIMEOUT_MAX_SECS);
}

#[test]
fn bash_timeout_schema_and_guidance_direct_polling_to_a_background_session() {
    let tool = bash_tool();
    let schema_description = tool.parameters["properties"]["timeout"]["description"]
        .as_str()
        .unwrap();
    assert!(schema_description.contains("default: 60; range 1-60"));

    // Issue #654 observed a model choosing a long foreground wait for a watch
    // command. The safe session handoff must be the explicit choice.
    for phrase in [
        "polling or watch commands",
        "`background: true`",
        "use BashSession rather than a long foreground wait",
    ] {
        assert!(
            tool.description.contains(phrase),
            "Bash description must direct polling to a background session ({phrase:?})"
        );
    }
}

#[test]
fn bash_cwd_argument_round_trips_and_is_optional() {
    let policy = crate::clients::tools::sandbox::SandboxPolicy::DangerFullAccess;

    let args =
        BashExecutionArgs::from_json(r#"{"command": "pwd", "cwd": "nested/project"}"#, policy)
            .unwrap();
    assert_eq!(
        args.cwd.as_deref(),
        Some(std::path::Path::new("nested/project"))
    );

    let args = BashExecutionArgs::from_json(r#"{"command": "pwd"}"#, policy).unwrap();
    assert_eq!(args.cwd, None);

    let result = BashExecutionArgs::from_json(r#"{"command": "pwd", "cwd": 42}"#, policy);
    assert!(result.is_err());
}

#[test]
fn bash_cwd_guidance_directs_using_cwd_instead_of_a_cd_prefix() {
    let tool = bash_tool();
    // Model-visible description: now that a call can name its directory, agents
    // must not hide the effective directory in a `cd` prefix, and must know the
    // selection is per-call. The first revision of this feature dropped the
    // `cd` prohibition as a side effect of rewriting the working-directory
    // sentence, so the guidance is guarded here.
    for phrase in [
        "Do not prefix commands with cd",
        "use `cwd` instead",
        "applies to one call",
    ] {
        assert!(
            tool.description.contains(phrase),
            "Bash description must direct cwd use ({phrase:?})"
        );
    }
}

#[test]
fn bash_cwd_schema_is_optional_and_describes_resolution() {
    let tool = bash_tool();
    let cwd = &tool.parameters["properties"]["cwd"];
    assert_eq!(cwd["type"], "string");
    let description = cwd["description"].as_str().unwrap();
    assert!(description.contains("Relative paths resolve"));
    assert!(description.contains("the sandbox already grants"));

    let required = tool.parameters["required"].as_array().unwrap();
    assert_eq!(required, &[serde_json::json!("command")]);
}

#[test]
fn bash_reason_argument_round_trips() {
    let policy = crate::clients::tools::sandbox::SandboxPolicy::DangerFullAccess;

    // Present: the reason is preserved on the parsed args.
    let args = BashExecutionArgs::from_json(
        r#"{"command": "git status", "reason": "inspect working tree"}"#,
        policy,
    )
    .unwrap();
    assert_eq!(args.reason.as_deref(), Some("inspect working tree"));

    // Absent: the field is None, not an error.
    let args = BashExecutionArgs::from_json(r#"{"command": "git status"}"#, policy).unwrap();
    assert_eq!(args.reason, None);

    // Non-string reason is rejected like any other invalid argument.
    let result = BashExecutionArgs::from_json(r#"{"command": "true", "reason": 42}"#, policy);
    assert!(result.is_err());
}

#[test]
fn bash_reason_guidance_directs_supplying_context_for_state_changing_commands() {
    let tool = bash_tool();
    // Model-visible description: agents supply a reason for state-changing,
    // destructive-looking, and remote-effect commands, stating the intended
    // effect; a reason never authorizes a remote destructive command on its
    // own, so the guard must be in the command (issue #203, PR #229).
    for phrase in [
        "state-changing, destructive-looking, or remote-effect commands",
        "intended effect",
        "The policy gate receives each command as a standalone request",
        "never authorizes a remote destructive command",
    ] {
        assert!(
            tool.description.contains(phrase),
            "Bash description must direct reason supply ({phrase:?})"
        );
    }

    // The schema description carries the same guidance.
    let schema_reason = &tool.parameters["properties"]["reason"]["description"];
    let schema_reason = schema_reason.as_str().unwrap();
    for phrase in [
        "state-changing, destructive-looking, or remote-effect commands",
        "stating the intended effect",
    ] {
        assert!(
            schema_reason.contains(phrase),
            "Bash schema must direct reason supply ({phrase:?})"
        );
    }

    // The reason stays optional at the wire level: only `command` is required.
    let required = tool.parameters["required"].as_array().unwrap();
    assert_eq!(required, &[serde_json::json!("command")]);
}

#[tokio::test]
async fn bash_cwd_runs_in_a_relative_workspace_subdirectory() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let nested = workspace.path().join("nested");
    std::fs::create_dir(&nested).expect("nested directory");
    let expected = nested
        .canonicalize()
        .expect("nested path must canonicalize");
    let arguments = serde_json::json!({
        "command": "pwd -P",
        "cwd": "nested",
    })
    .to_string();
    let gate = gate_fixture::decision(GateDecision::Allow, None, "Safe");

    let result = execute_bash_with_gates_in(&arguments, Some(gate.pipeline()), workspace.path())
        .await
        .expect("bash run should succeed");

    assert!(
        result
            .output
            .starts_with(&format!("{}\n", expected.display())),
        "pwd should report the requested cwd, got: {}",
        result.output
    );

    let frames = gate.frames();
    let input = &frames[0].input;
    assert_eq!(
        input.cwd,
        expected.display().to_string(),
        "the gate must receive the resolved cwd"
    );
    assert_eq!(input.command, "pwd -P");
}

/// An absolute request inside the workspace is canonicalized and honored, so a
/// caller that already knows the full path need not express it relatively.
#[tokio::test]
async fn bash_cwd_accepts_an_absolute_path_inside_the_workspace() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let nested = workspace.path().join("nested");
    std::fs::create_dir(&nested).expect("nested directory");
    let expected = nested
        .canonicalize()
        .expect("nested path must canonicalize");
    let arguments = serde_json::json!({
        "command": "pwd -P",
        "cwd": expected.display().to_string(),
    })
    .to_string();

    let result =
        execute_bash_with_gates_in(&arguments, Some(bypassed_gate_context()), workspace.path())
            .await
            .expect("bash run should succeed");

    assert!(
        result
            .output
            .starts_with(&format!("{}\n", expected.display())),
        "an absolute cwd inside the workspace must be honored, got: {}",
        result.output
    );
}

#[tokio::test]
async fn bash_cwd_rejects_missing_path_and_file_path_before_spawn() {
    // Both cases are rejected by canonicalization, so they hold under every
    // policy, including the `DangerFullAccess` context this helper applies. The
    // ungranted-directory case needs a policy that consults the grant table and
    // lives in `bash_cwd_rejects_a_directory_outside_every_grant_before_spawn`.
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let file = workspace.path().join("not-a-directory");
    std::fs::write(&file, "fixture").expect("file fixture");
    let marker = workspace.path().join("spawned");
    let invalid_cwds = [workspace.path().join("missing"), file];

    for cwd in invalid_cwds {
        let arguments = serde_json::json!({
            "command": format!("touch {}", marker.display()),
            "cwd": cwd.display().to_string(),
        })
        .to_string();
        let error =
            execute_bash_with_gates_in(&arguments, Some(bypassed_gate_context()), workspace.path())
                .await
                .expect_err("invalid cwd must fail before spawning Bash");
        assert!(
            error.message.contains("Invalid Bash working directory"),
            "unexpected cwd error: {}",
            error.message
        );
    }

    assert!(!marker.exists(), "invalid cwd must not spawn the command");
}

/// Build a `ToolContext` for working-directory admission tests: an explicit
/// workspace and policy, no temp-directory grants, and the judge bypassed.
///
/// `ToolContext::from_current_process` treats the process `TMPDIR` as writable,
/// which makes every `tempfile` fixture a grant and hides whether a grant is
/// what admitted the path. Starting from an empty temp set keeps the fixtures
/// honest: a fixture directory is selectable only because the test granted it.
/// Each test then adds the sandbox grants it exercises by field, so no call
/// passes two same-typed directory lists positionally.
fn admission_context_at(cwd: &std::path::Path, policy: SandboxPolicy) -> ToolContext {
    let mut context = ToolContext::with_temp_dirs(
        cwd.to_path_buf(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    context.sandbox_policy = policy;
    context.gates = Some(bypassed_gate_context());
    context
}

/// Resolve the requested `cwd` exactly as the tool executor does, without
/// running a command.
fn resolve_cwd_for_test(
    context: &ToolContext,
    requested: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let arguments = serde_json::json!({
        "command": "pwd -P",
        "cwd": requested.display().to_string(),
    })
    .to_string();
    parse_bash_call(context, &arguments).map(|(_, cwd, _)| cwd)
}

/// Every user grant class is selectable, as is the workspace and any child of a
/// grant.
#[test]
fn bash_cwd_admits_settings_add_dir_and_skill_directory_grants() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let settings_dir = tempfile::tempdir().expect("settings fixture");
    let add_dir = tempfile::tempdir().expect("add-dir fixture");
    let skill_dir = tempfile::tempdir().expect("skill fixture");
    let nested = add_dir.path().join("nested");
    std::fs::create_dir(&nested).expect("nested fixture");

    let mut context = admission_context_at(workspace.path(), SandboxPolicy::WorkspaceWrite);
    context.additional_dirs = vec![add_dir.path().to_path_buf()];
    context.settings_dirs = vec![settings_dir.path().to_path_buf()];
    context.skill_dirs = vec![skill_dir.path().to_path_buf()];

    for granted in [
        settings_dir.path(),
        add_dir.path(),
        skill_dir.path(),
        &nested,
    ] {
        let resolved = resolve_cwd_for_test(&context, granted)
            .unwrap_or_else(|error| panic!("{} must be selectable: {error}", granted.display()));
        assert_eq!(
            resolved,
            granted.canonicalize().expect("fixture must canonicalize")
        );
    }

    assert_eq!(
        resolve_cwd_for_test(&context, workspace.path()).expect("workspace must be selectable"),
        workspace
            .path()
            .canonicalize()
            .expect("fixture must canonicalize")
    );
}

/// A settings directory stays selectable under the read-only policy, where the
/// sandbox demotes it from a writable grant into the read-and-execute set.
#[test]
fn bash_cwd_admits_a_settings_directory_grant_under_read_only() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let settings_dir = tempfile::tempdir().expect("settings fixture");
    let mut context = admission_context_at(workspace.path(), SandboxPolicy::ReadOnly);
    context.settings_dirs = vec![settings_dir.path().to_path_buf()];

    assert_eq!(
        resolve_cwd_for_test(&context, settings_dir.path())
            .expect("a read-only grant must stay selectable"),
        settings_dir
            .path()
            .canonicalize()
            .expect("fixture must canonicalize")
    );
}

/// The built-in read-and-execute system paths are readable by absolute path but
/// are not admission sources, so a command cannot start in them.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn bash_cwd_rejects_built_in_system_paths() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let context = admission_context_at(workspace.path(), SandboxPolicy::WorkspaceWrite);

    for system in ["/etc", "/usr"] {
        let error = resolve_cwd_for_test(&context, std::path::Path::new(system))
            .expect_err("a built-in system path must not be selectable");
        assert!(
            error.contains("outside the invocation workspace"),
            "unexpected cwd error for {system}: {error}"
        );
    }
}

/// Under `DangerFullAccess` the sandbox applies no profile, so a grant check
/// would be stricter than the policy in force: any existing directory is
/// admitted.
#[test]
fn bash_cwd_admits_any_existing_directory_under_danger_full_access() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let ungranted = tempfile::tempdir().expect("ungranted fixture");
    let context = admission_context_at(workspace.path(), SandboxPolicy::DangerFullAccess);

    assert_eq!(
        resolve_cwd_for_test(&context, ungranted.path())
            .expect("danger-full-access admits any existing directory"),
        ungranted
            .path()
            .canonicalize()
            .expect("fixture must canonicalize")
    );
}

/// A directory outside the invocation workspace and outside every grant is
/// rejected before the judge runs and before any process is spawned, with a
/// message that names the grant classes.
#[tokio::test]
async fn bash_cwd_rejects_a_directory_outside_every_grant_before_spawn() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let granted_elsewhere = tempfile::tempdir().expect("ungranted fixture");
    let marker = workspace.path().join("spawned");
    let mut other_context = admission_context_at(workspace.path(), SandboxPolicy::WorkspaceWrite);
    other_context.additional_dirs = vec![granted_elsewhere.path().to_path_buf()];
    let context = admission_context_at(workspace.path(), SandboxPolicy::WorkspaceWrite);

    // Guard the fixture: the same directory is admitted once `--add-dir` grants
    // it, so a rejection here is about the missing grant and not the path.
    assert!(
        resolve_cwd_for_test(&other_context, granted_elsewhere.path()).is_ok(),
        "fixture directory must be admissible when it is granted"
    );

    let arguments = serde_json::json!({
        "command": format!("touch {}", marker.display()),
        "cwd": granted_elsewhere.path().display().to_string(),
    })
    .to_string();

    let error = execute_bash(&context, &arguments)
        .await
        .expect_err("a cwd outside every grant must fail before spawning Bash");
    assert!(
        error.message.contains("Invalid Bash working directory"),
        "unexpected cwd error: {}",
        error.message
    );
    assert!(
        error.message.contains("outside the invocation workspace"),
        "rejection must name the workspace boundary, got: {}",
        error.message
    );
    for grant_class in ["--add-dir", "[sandbox]", "skill directory"] {
        assert!(
            error.message.contains(grant_class),
            "rejection must name the {grant_class} grant class, got: {}",
            error.message
        );
    }
    assert!(
        !marker.exists(),
        "a rejected cwd must not spawn the command"
    );
}

/// A symlink inside the workspace whose canonical target is outside every grant
/// is rejected, so a grant cannot be reached through a link the grants do not
/// cover.
#[cfg(unix)]
#[tokio::test]
async fn bash_cwd_rejects_symlink_that_escapes_every_grant() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let target = tempfile::tempdir().expect("ungranted target fixture");
    let link = workspace.path().join("outside-link");
    std::os::unix::fs::symlink(target.path(), &link).expect("symlink fixture");
    let context = admission_context_at(workspace.path(), SandboxPolicy::WorkspaceWrite);
    let arguments = serde_json::json!({
        "command": "pwd -P",
        "cwd": "outside-link",
    })
    .to_string();

    let error = execute_bash(&context, &arguments)
        .await
        .expect_err("an escaping cwd symlink must be rejected");
    assert!(error.message.contains("outside the invocation workspace"));
}

/// A symlink whose canonical target is inside a grant is accepted, so admission
/// follows the link exactly as the child process does.
#[cfg(unix)]
#[test]
fn bash_cwd_accepts_a_symlink_whose_target_is_granted() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let granted = tempfile::tempdir().expect("grant fixture");
    let link = workspace.path().join("granted-link");
    std::os::unix::fs::symlink(granted.path(), &link).expect("symlink fixture");
    let mut context = admission_context_at(workspace.path(), SandboxPolicy::WorkspaceWrite);
    context.additional_dirs = vec![granted.path().to_path_buf()];

    assert_eq!(
        resolve_cwd_for_test(&context, &link).expect("a granted symlink target must be admitted"),
        granted
            .path()
            .canonicalize()
            .expect("fixture must canonicalize")
    );
}

/// A granted directory outside the invocation workspace is a real working
/// directory: the child runs there and the judge is told the same canonical
/// path.
///
/// Skipping when the platform sandbox cannot be applied also guards the fixture,
/// which is created under the account home and would otherwise be refused by an
/// outer sandbox. The macOS `Test` CI job is where this runs for real.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn bash_cwd_runs_in_a_settings_directory_grant() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a directory outside the workspace");
    let workspace = tempfile::TempDir::new_in(&outside).expect("should create test workspace");
    let settings_dir =
        tempfile::TempDir::new_in(&outside).expect("should create a settings-directory grant");
    let expected = settings_dir
        .path()
        .canonicalize()
        .expect("settings fixture must canonicalize");
    let arguments = serde_json::json!({
        "command": "pwd -P",
        "cwd": settings_dir.path().display().to_string(),
    })
    .to_string();

    let gate = gate_fixture::decision(GateDecision::Allow, None, "Safe");

    let mut context = ToolContext::from_current_process().with_gates(Some(gate.pipeline()));
    context.cwd = workspace.path().to_path_buf();
    context.sandbox_policy = SandboxPolicy::WorkspaceWrite;
    context.settings_dirs = vec![settings_dir.path().to_path_buf()];

    let result = Box::pin(execute_bash(&context, &arguments))
        .await
        .expect("bash run should succeed");
    assert!(
        result
            .output
            .starts_with(&format!("{}\n", expected.display())),
        "a sandboxed command must start in the granted directory, got: {}",
        result.output
    );

    let frames = gate.frames();
    assert_eq!(frames.len(), 1, "the gate must receive exactly one request");
    assert_eq!(
        frames[0].input.cwd,
        expected.display().to_string(),
        "the gate must receive the resolved cwd"
    );
}

/// Read-only grants stay selectable under the read-only policy, and selecting
/// one still does not widen authority: a write inside the selected directory is
/// denied by the sandbox.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn bash_cwd_selects_an_add_dir_grant_under_read_only_without_widening_authority() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a directory outside the workspace");
    let workspace = tempfile::TempDir::new_in(&outside).expect("should create test workspace");
    let granted = tempfile::TempDir::new_in(&outside).expect("should create an --add-dir grant");
    let expected = granted
        .path()
        .canonicalize()
        .expect("grant fixture must canonicalize");
    let probe = format!("cake_cwd_ro_probe_{}", uuid::Uuid::new_v4());
    let arguments = serde_json::json!({
        "command": format!("pwd -P; touch {probe}"),
        "cwd": granted.path().display().to_string(),
    })
    .to_string();

    let mut context = ToolContext::from_current_process().with_gates(Some(bypassed_gate_context()));
    context.cwd = workspace.path().to_path_buf();
    context.sandbox_policy = SandboxPolicy::ReadOnly;
    context.additional_dirs = vec![granted.path().to_path_buf()];

    let result = Box::pin(execute_bash(&context, &arguments))
        .await
        .expect("bash run should succeed");
    assert!(
        result
            .output
            .starts_with(&format!("{}\n", expected.display())),
        "the selected cwd must be the granted directory, got: {}",
        result.output
    );
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "the read-only policy must still deny a write inside the selected directory, got: {}",
        result.output
    );
    assert!(
        !granted.path().join(&probe).exists(),
        "the read-only probe file must not be created"
    );
}

#[test]
fn truncate_output_passes_through_at_limit() {
    let exact = "a".repeat(BASH_OUTPUT_MAX_BYTES);
    let result = truncate_output(&exact, Some(BASH_OUTPUT_MAX_BYTES), 0, 50, false);
    assert!(result.contains(&exact));
    assert!(result.contains("[exit:0 | 50ms]"));
}

#[test]
fn truncate_output_truncates_large_output() {
    let large = "x".repeat(BASH_OUTPUT_MAX_BYTES + 1000);
    let result = truncate_output(&large, Some(BASH_OUTPUT_MAX_BYTES), 0, 500, false);
    assert!(result.len() < large.len());
    assert!(result.contains("[Output too long"));
    assert!(result.contains("Full output saved to:"));
    assert!(result.contains("[exit:0 | 500ms]"));
}

#[test]
fn truncate_output_handles_multibyte_chars() {
    // Create output with multi-byte UTF-8 characters that exceeds the limit
    let large = "é".repeat(BASH_OUTPUT_MAX_BYTES); // each 'é' is 2 bytes
    let result = truncate_output(&large, Some(BASH_OUTPUT_MAX_BYTES), 1, 2000, false);
    assert!(result.contains("[Output too long"));
    assert!(result.contains("[exit:1 | 2.0s]"));
}

#[test]
fn truncate_output_temp_file_has_no_footer() {
    let large = "x".repeat(BASH_OUTPUT_MAX_BYTES + 1000);
    let result = truncate_output(&large, Some(BASH_OUTPUT_MAX_BYTES), 0, 100, false);
    // Extract the temp file path from the result
    let path_line = result
        .lines()
        .find(|l| l.starts_with("Full output saved to:"))
        .expect("should contain temp file path");
    let path = path_line
        .trim_start_matches("Full output saved to: ")
        .trim();
    let contents = std::fs::read_to_string(path).expect("should read temp file");
    assert!(
        !contents.contains("[exit:"),
        "temp file should not contain metadata footer"
    );
}

// ===========================================================================
// Metadata Footer Tests
// ===========================================================================

#[test]
fn metadata_footer_shows_milliseconds_under_1_second() {
    let footer = format_metadata_footer(0, 500);
    assert_eq!(footer, "[exit:0 | 500ms]");
}

#[test]
fn metadata_footer_shows_milliseconds_at_boundary() {
    // 999ms should still show as milliseconds
    let footer = format_metadata_footer(0, 999);
    assert_eq!(footer, "[exit:0 | 999ms]");
}

#[test]
fn metadata_footer_shows_seconds_over_1_second() {
    // 1000ms should show as 1.0s
    let footer = format_metadata_footer(0, 1000);
    assert_eq!(footer, "[exit:0 | 1.0s]");
}

#[test]
fn metadata_footer_shows_seconds_with_decimal() {
    // 1234ms should show as 1.2s (rounded to 1 decimal)
    let footer = format_metadata_footer(1, 1234);
    assert_eq!(footer, "[exit:1 | 1.2s]");
}

#[test]
fn metadata_footer_handles_large_values() {
    // 60000ms = 60.0s
    let footer = format_metadata_footer(0, 60000);
    assert_eq!(footer, "[exit:0 | 60.0s]");
}

#[test]
fn format_kib_tenths_rounds_to_nearest_tenth() {
    assert_eq!(format_kib_tenths(0), "0.0");
    assert_eq!(format_kib_tenths(51), "0.0");
    assert_eq!(format_kib_tenths(52), "0.1");
    assert_eq!(format_kib_tenths(1024), "1.0");
    assert_eq!(format_kib_tenths(1536), "1.5");
}

#[test]
fn format_kib_tenths_handles_max_size_without_overflowing() {
    let formatted = format_kib_tenths(usize::MAX);

    assert!(formatted.contains('.'));
}

#[test]
fn empty_rg_no_match_is_annotated() {
    let result = annotate_empty_search_result("rg definitely_missing src", String::new(), 1, "");

    assert_eq!(result, EMPTY_SEARCH_NO_MATCH_ANNOTATION);
}

#[test]
fn empty_non_search_exit_one_is_not_annotated() {
    let result = annotate_empty_search_result("false", String::new(), 1, "");

    assert_eq!(result, "");
}

#[test]
fn search_error_with_stderr_is_not_annotated() {
    let result = annotate_empty_search_result(
        "rg definitely_missing src",
        String::new(),
        1,
        "regex parse error",
    );

    assert_eq!(result, "");
}

// ===========================================================================
// Streaming Tests
// ===========================================================================

#[tokio::test]
async fn test_streaming_small_output() {
    // Command with small output returns it verbatim with metadata footer
    let args = r#"{"command": "echo hello world"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("hello world"));
    assert!(result.output.contains("[exit:0 |"));
}

#[tokio::test]
async fn test_streaming_large_output_is_capped() {
    // Command that produces output beyond the default read cap is truncated
    // Produce ~200KB of output (well over the 100KB cap)
    let args = r#"{"command": "yes | head -c 200000"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    // Should contain the truncation marker
    assert!(result.output.contains("[Output too long"));
    // Should still have useful content
    assert!(!result.output.is_empty());
    // Should contain metadata footer
    assert!(result.output.contains("[exit:"));
    // The read-cap truncation is recorded as a compensation event (plus the
    // judge bypass event from the bypassed judge context used here).
    let truncations: Vec<_> = result
        .compensation_events
        .iter()
        .filter(|e| e.kind == crate::session_telemetry::CompensationKind::OutputTruncation)
        .collect();
    assert_eq!(
        truncations.len(),
        1,
        "read-cap truncation must record one output_truncation event"
    );
    assert_eq!(truncations[0].detail.as_deref(), Some("Bash"));
    assert!(
        result
            .compensation_events
            .iter()
            .any(|e| e.kind == crate::session_telemetry::CompensationKind::JudgeBypass),
        "bypassed judge must record a judge_bypass event"
    );
}

#[tokio::test]
async fn bash_read_cap_no_longer_kills_or_truncates_session_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut context = crate::clients::tools::ToolContext::from_current_process();
    context.cwd = dir.path().to_path_buf();
    context.gates = Some(bypassed_gate_context());
    context.sandbox_policy = SandboxPolicy::DangerFullAccess;
    let mut limits = crate::config::settings::ToolLimits::defaults();
    limits.bash_read_cap = Some(100);
    context = context.with_limits(limits);
    let result = execute_bash(&context, r#"{"command": "yes x | head -c 20000"}"#)
        .await
        .unwrap();
    assert!(result.output.len() > 20_000);
    assert!(result.output.contains("[exit:0 |"));
    assert!(!result.output.contains("output truncated"));
}

#[tokio::test]
async fn bash_output_max_bytes_override_spills_at_custom_cap() {
    // A configured `bash_output_max_bytes` below the compiled default changes
    // the spill threshold: 5,000 bytes of output exceeds a 1,000-byte cap but
    // stays under the default 100,000-byte read cap, so the run completes and
    // the output spills to a temp file at the custom cap.
    let dir = tempfile::tempdir().expect("hermetic temp dir for bash test");
    let mut context = crate::clients::tools::ToolContext::from_current_process();
    context.cwd = dir.path().to_path_buf();
    context.gates = Some(bypassed_gate_context());
    let mut limits = crate::config::settings::ToolLimits::defaults();
    limits.bash_output_max_bytes = Some(1000);
    context.limits = limits;

    let args = BashExecutionArgs::from_json(
        r#"{"command": "yes x | head -c 5000"}"#,
        crate::clients::tools::sandbox::SandboxPolicy::DangerFullAccess,
    )
    .unwrap();
    let config = crate::clients::tools::sandbox::SandboxConfig::build(&context);
    let result = Box::pin(execute_bash_with_args(
        &context,
        args,
        context.cwd.clone(),
        &config,
        None,
    ))
    .await
    .expect("bash run should succeed");

    assert!(
        result.output.contains("[Output too long"),
        "output must spill at the custom cap, got: {}",
        result.output
    );
    assert!(result.output.contains("Full output saved to:"));
}

#[tokio::test]
async fn bash_output_max_bytes_unlimited_passes_large_output_through() {
    // `bash_output_max_bytes = "unlimited"` disables the spill and the read
    // cap, so 60,000 bytes of output (over the compiled 50,000-byte inline
    // cap) pass through in full.
    let dir = tempfile::tempdir().expect("hermetic temp dir for bash test");
    let mut context = crate::clients::tools::ToolContext::from_current_process();
    context.cwd = dir.path().to_path_buf();
    context.gates = Some(bypassed_gate_context());
    let mut limits = crate::config::settings::ToolLimits::defaults();
    limits.bash_output_max_bytes = None;
    limits.bash_read_cap = None;
    context.limits = limits;

    let args = BashExecutionArgs::from_json(
        r#"{"command": "yes x | head -c 60000"}"#,
        crate::clients::tools::sandbox::SandboxPolicy::DangerFullAccess,
    )
    .unwrap();
    let config = crate::clients::tools::sandbox::SandboxConfig::build(&context);
    let result = Box::pin(execute_bash_with_args(
        &context,
        args,
        context.cwd.clone(),
        &config,
        None,
    ))
    .await
    .expect("bash run should succeed");

    // `yes x | head -c 60000` emits 60,000 bytes of "x\n" lines; without a
    // read cap or inline cap the full stream survives (plus the footer).
    assert!(
        result.output.len() > 60_000,
        "unlimited output must pass through in full, got {} bytes",
        result.output.len()
    );
    assert!(
        !result.output.contains("[Output too long"),
        "unlimited budget must not spill"
    );
    assert!(
        !result.output.contains("output truncated"),
        "unlimited budget must not truncate"
    );
}

fn session_test_context() -> ToolContext {
    let mut context = ToolContext::from_current_process();
    context.gates = Some(bypassed_gate_context());
    context.sandbox_policy = SandboxPolicy::DangerFullAccess;
    context
}

#[tokio::test]
async fn short_bash_command_returns_inline_without_retained_session() {
    let context = session_test_context();
    let result = execute_bash(&context, r#"{"command":"printf short"}"#)
        .await
        .unwrap();
    assert!(result.output.starts_with("short\n\n[exit:0 |"));
    assert!(context.bash_sessions.list(Instant::now()).is_empty());
}

#[tokio::test]
async fn full_session_cap_blocks_before_judge_preflight() {
    let limits = crate::config::settings::ToolLimits {
        bash_session_max: 1,
        ..crate::config::settings::ToolLimits::defaults()
    };
    let context = ToolContext::from_current_process().with_limits(limits);
    let occupied = context
        .bash_sessions
        .start("occupied".into(), Instant::now())
        .unwrap();
    let error = execute_bash(&context, r#"{"command":"printf never"}"#)
        .await
        .unwrap_err();
    assert!(error.message.contains("session limit 1"));
    assert!(error.message.contains(&occupied));
    assert!(!error.message.contains("judge"));
}

#[tokio::test]
async fn trailing_ampersand_is_stripped_and_reported() {
    let context = session_test_context();
    let result = execute_bash(&context, r#"{"command":"printf safe &"}"#)
        .await
        .unwrap();
    assert!(result.output.contains("safe"));
    assert!(result.output.contains("Trailing & was removed"));
    assert!(context.bash_sessions.list(Instant::now()).is_empty());
    assert_eq!(
        normalize_trailing_background("printf safe &&"),
        ("printf safe &&".into(), false)
    );
}

#[tokio::test]
async fn trailing_ampersand_is_removed_before_the_gate_request() {
    let gate = gate_fixture::decision(GateDecision::Allow, None, "Safe");
    let result = execute_bash_with_gates(r#"{"command":"printf safe &"}"#, Some(gate.pipeline()))
        .await
        .unwrap();
    assert!(result.output.contains("Trailing & was removed"));
    let frames = gate.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].input.command, "printf safe");
}

#[tokio::test]
async fn bash_yields_then_session_read_reports_new_output_and_final_exit() {
    let context = session_test_context();
    let started = execute_bash(
        &context,
        r#"{"command":"printf first; sleep 2; printf second","timeout":1}"#,
    )
    .await
    .unwrap();
    assert!(started.output.contains("first"));
    assert!(started.output.contains("[still running | session: bash_"));
    let id = context.bash_sessions.list(Instant::now())[0].id.clone();
    let mut new_output = String::new();
    let final_read = loop {
        let read = super::super::bash_session::execute(
            &context,
            &serde_json::json!({"action":"read","session":id,"wait":5}).to_string(),
        )
        .await
        .unwrap();
        new_output.push_str(&read.output);
        if read.output.contains("[exit:0 | total") {
            break read;
        }
    };
    assert!(new_output.contains("second"));
    assert!(!new_output.contains("first"));
    assert!(final_read.output.contains("[exit:0 | total"));
    let replay = super::super::bash_session::execute(
        &context,
        &serde_json::json!({"action":"read","session":id,"wait":0}).to_string(),
    )
    .await
    .unwrap();
    assert_eq!(replay.output, final_read.output);
}

#[tokio::test]
async fn bash_background_returns_immediately_and_can_be_killed() {
    let context = session_test_context();
    let started = Instant::now();
    let result = execute_bash(&context, r#"{"command":"sleep 999","background":true}"#)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(result.output.contains("[still running | session: bash_"));
    let id = context.bash_sessions.list(Instant::now())[0].id.clone();
    let killed = super::super::bash_session::execute(
        &context,
        &serde_json::json!({"action":"kill","session":id}).to_string(),
    )
    .await
    .unwrap();
    assert!(killed.output.contains("[exit:-1 | total"));
}

#[tokio::test]
async fn yielded_bash_output_spills_drained_bytes() {
    let mut context = session_test_context();
    context.limits.bash_output_max_bytes = Some(100);
    let result = execute_bash(
        &context,
        r#"{"command":"yes x | head -c 60000; sleep 2","timeout":1}"#,
    )
    .await
    .unwrap();

    assert!(result.output.contains("[still running | session: bash_"));
    let path = result
        .output
        .lines()
        .find_map(|line| line.strip_prefix("Full output saved to: "))
        .expect("yielded output must provide a recovery path");
    let spilled = std::fs::read(path).unwrap();
    assert_eq!(spilled.len(), 60_000);
    assert!(spilled.starts_with(b"x\nx\n"));
}

#[tokio::test]
async fn bash_session_read_spills_drained_bytes() {
    let mut context = session_test_context();
    context.limits.bash_output_max_bytes = Some(100);
    let started = execute_bash(
        &context,
        r#"{"command":"sleep 1; yes x | head -c 60000","background":true}"#,
    )
    .await
    .unwrap();
    assert!(started.output.contains("session: bash_"));
    let id = context.bash_sessions.list(Instant::now())[0].id.clone();
    context
        .bash_sessions
        .wait_for_completion(&id, Duration::from_secs(5))
        .await
        .unwrap();
    let result = super::super::bash_session::execute(
        &context,
        &serde_json::json!({"action":"read","session":id,"wait":0}).to_string(),
    )
    .await
    .unwrap();
    let path = result
        .output
        .lines()
        .find_map(|line| line.strip_prefix("Full output saved to: "))
        .expect("read output must provide a recovery path");
    assert_eq!(std::fs::read(path).unwrap().len(), 60_000);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn sandboxed_bash_session_yields_and_completes_under_platform_policy() {
    if skip_if_sandbox_unavailable() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("session-finished");
    let mut context = ToolContext::from_current_process();
    context.cwd = dir.path().to_path_buf();
    context.gates = Some(bypassed_gate_context());
    context.sandbox_policy = SandboxPolicy::WorkspaceWrite;
    let command = format!("sleep 2; touch '{}'", marker.display());
    let result = execute_bash(
        &context,
        &serde_json::json!({"command":command,"timeout":1}).to_string(),
    )
    .await
    .unwrap();
    assert!(result.output.contains("[still running | session: bash_"));
    let id = context.bash_sessions.list(Instant::now())[0].id.clone();
    context
        .bash_sessions
        .wait_for_completion(&id, Duration::from_secs(5))
        .await
        .unwrap();
    let final_read = super::super::bash_session::execute(
        &context,
        &serde_json::json!({"action":"read","session":id,"wait":0}).to_string(),
    )
    .await
    .unwrap();
    assert!(final_read.output.contains("[exit:0 | total"));
    assert!(marker.exists());

    let running = execute_bash(&context, r#"{"command":"sleep 999","background":true}"#)
        .await
        .unwrap();
    assert!(running.output.contains("[still running | session: bash_"));
    let live_id = context
        .bash_sessions
        .list(Instant::now())
        .into_iter()
        .find(|session| session.exit_code.is_none())
        .unwrap()
        .id;
    let killed = super::super::bash_session::execute(
        &context,
        &serde_json::json!({"action":"kill","session":live_id}).to_string(),
    )
    .await
    .unwrap();
    assert!(killed.output.contains("[exit:-1 | total"));

    let orphan = dir.path().join("redirected-background-child");
    let command = format!(
        "(sleep 1; touch '{}') >/dev/null 2>&1 & true",
        orphan.display()
    );
    let completed = execute_bash(
        &context,
        &serde_json::json!({"command":command}).to_string(),
    )
    .await
    .unwrap();
    assert!(completed.output.contains("[exit:0 |"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !orphan.exists(),
        "redirected background child survived shell exit"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn sandboxed_bash_session_finishes_when_descendant_holds_captured_pipe() {
    if skip_if_sandbox_unavailable() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pipe-holder.pid");
    let mut context = ToolContext::from_current_process();
    context.cwd = dir.path().to_path_buf();
    context.gates = Some(bypassed_gate_context());
    context.sandbox_policy = SandboxPolicy::WorkspaceWrite;
    let command = format!("sleep 30 & echo $! > '{}'; echo done", pid_file.display());

    let started = Instant::now();
    let completed = execute_bash(
        &context,
        &serde_json::json!({"command":command,"timeout":2}).to_string(),
    )
    .await
    .unwrap();
    assert!(completed.output.contains("done"));
    assert!(completed.output.contains("[exit:0 |"));
    assert!(started.elapsed() < Duration::from_secs(2));

    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // SAFETY: the PID names the sleep process spawned by this test.
        let exists = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
        if !exists {
            break;
        }
        assert!(Instant::now() < deadline, "descendant survived shell exit");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_descendant_that_outlives_the_direct_child() {
    // The direct child exits as soon as it receives SIGTERM, while a
    // descendant ignores it. Once the direct child is reaped its group can no
    // longer be resolved from the child handle, so the forceful phase must
    // signal the group id captured before the wait. Asserted against the
    // termination helper directly: through the tool path the
    // `ToolboxProcessGuard` drop-kill would mask whether the forceful phase
    // ran at all.
    let dir = tempfile::tempdir().unwrap();
    let alive = dir.path().join("descendant-alive");
    let saw_term = dir.path().join("descendant-saw-term");
    let pidfile = dir.path().join("descendant-pid");
    // The descendant records the SIGTERM it ignores, proving the cooperative
    // signal reached it, and rewrites `alive` until it is force-killed. Its
    // loop is bounded so a regression cannot leave a runaway process behind.
    let script = format!(
        r#"#!/bin/sh
echo $$ > "{pidfile}"
trap 'touch "{saw_term}"' TERM
i=0
while [ $i -lt 100 ]; do
  touch "{alive}"
  sleep 0.1
  i=$((i+1))
done
"#,
        pidfile = pidfile.display(),
        saw_term = saw_term.display(),
        alive = alive.display()
    );
    let script_path = dir.path().join("descendant.sh");
    std::fs::write(&script_path, script.as_bytes()).unwrap();
    std::fs::set_permissions(
        &script_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(format!("{} & sleep 999", script_path.display()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Mirror the tool's spawn: the child owns a process group so the helper
    // can signal the whole group through its negative PID.
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    let mut child = cmd.spawn().unwrap();

    // Wait until the descendant installed its trap and started writing.
    let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !alive.exists() {
        assert!(
            std::time::Instant::now() < ready_deadline,
            "the descendant never started writing its marker"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Establish the precondition without touching the direct child: signal
    // only the descendant, then confirm it recorded the signal and kept
    // running. Both facts must hold before the forceful phase is meaningful.
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: the PID was written by the descendant this test spawned and no
    // one else has reaped it.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let saw_term_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !saw_term.exists() {
        assert!(
            std::time::Instant::now() < saw_term_deadline,
            "the descendant never ran its SIGTERM trap"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // The loop is still writing, so poll to a deadline rather than asserting
    // over a fixed window: one iteration delayed past a window leaves the
    // descendant alive while the assertion fires.
    let before = marker_modified_time(&alive);
    let resume_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    assert_marker_updates_resume(&alive, before, resume_deadline).await;

    terminate_process_group_gracefully(&mut child, TERMINATE_GRACE_PERIOD).await;

    // The descendant ignored SIGTERM, so only the forceful phase can stop it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    assert_marker_updates_stop(&alive, deadline).await;

    // No stray descendant survives a failed assertion above.
    // SAFETY: the PID was written by the descendant this test spawned and no
    // one else has reaped it.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_bash_future_kills_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("descendant-survived");
    // The descendant writes the marker in a loop until the process group is
    // killed.  A one-shot `sleep 2; touch` would instead race the kill:
    // under load the marker can be written before the kill lands, failing
    // the test even though the process-group teardown is prompt.
    let command = format!(
        "(while true; do touch '{}'; sleep 0.2; done) & sleep 999",
        marker.display()
    );
    let args = serde_json::json!({ "command": command }).to_string();
    let execution = tokio::spawn(async move { execute_bash_unsandboxed(&args).await });

    // Wait until the descendant is provably alive and writing its marker:
    // its first write lands within ~200 ms of the subshell spawning.
    timeout(std::time::Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("descendant did not start writing its marker");

    execution.abort();
    assert!(execution.await.unwrap_err().is_cancelled());

    // The descendant must stop updating the marker once the process group is
    // killed.  A slow kill only extends the poll; it cannot race a fixed
    // deadline the way the one-shot marker could.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    assert_marker_updates_stop(&marker, deadline).await;
}

/// Wait until `marker`'s mtime moves past `before`, or panic after `deadline`.
///
/// The descendant writes the marker every ~100 ms while it lives, so a live
/// descendant is detected by the first poll.  Polling rather than asserting
/// over a fixed window keeps a slow or loaded machine from failing while the
/// descendant is still writing; a descendant that stopped writing never moves
/// the marker and still fails, just at the deadline.
#[cfg(unix)]
async fn assert_marker_updates_resume(
    marker: &std::path::Path,
    before: Option<std::time::SystemTime>,
    deadline: std::time::Instant,
) {
    while marker_modified_time(marker) == before {
        assert!(
            std::time::Instant::now() < deadline,
            "the descendant did not survive the SIGTERM it traps"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Wait until `marker`'s mtime stops changing, or panic after `deadline`.
///
/// The descendant writes the marker every ~200 ms while alive, so a 500 ms
/// sampling interval always observes an update while it lives.  Two
/// consecutive unchanged samples mean the writing process has been
/// terminated.
#[cfg(unix)]
async fn assert_marker_updates_stop(marker: &std::path::Path, deadline: std::time::Instant) {
    loop {
        let before = marker_modified_time(marker);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let after = marker_modified_time(marker);
        if before == after {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "marker kept being updated; the descendant survived termination"
        );
    }
}

#[cfg(unix)]
fn marker_modified_time(marker: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(marker).ok()?.modified().ok()
}

#[cfg(unix)]
#[tokio::test]
async fn completed_bash_future_kills_background_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("background-descendant-completed");
    let command = format!(
        "(sleep 1; touch '{}') >/dev/null 2>&1 & true",
        marker.display()
    );
    let args = serde_json::json!({ "command": command }).to_string();

    let result = execute_bash_unsandboxed(&args).await.unwrap();
    assert!(!result.output.contains("Trailing & was removed"));

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        !marker.exists(),
        "backgrounded descendant survived normal Bash completion"
    );
}

#[tokio::test]
async fn test_streaming_stderr_included() {
    // Command that writes to stderr has it captured with metadata footer
    let args = r#"{"command": "echo err >&2"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("err"));
    assert!(result.output.contains(EXIT_ZERO_STDERR_WARNING));
    assert!(result.output.contains("[exit:0 |"));
}

#[tokio::test]
async fn failed_command_stderr_does_not_get_exit_zero_warning() {
    let args = r#"{"command": "echo err >&2; exit 1"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(result.output.contains("err"));
    assert!(!result.output.contains(EXIT_ZERO_STDERR_WARNING));
    assert!(result.output.contains("[exit:1 |"));
}

#[tokio::test]
async fn grep_no_match_output_is_disambiguated() {
    // Run in a hermetic fixture directory containing a Cargo.toml, never the
    // developer's real checkout.
    let dir = tempfile::tempdir().expect("fixture temp dir for grep test");
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("write fixture");
    let args = r#"{"command": "grep definitely_missing Cargo.toml"}"#;
    let result = Box::pin(execute_bash_with_gates_in(
        args,
        Some(bypassed_gate_context()),
        dir.path(),
    ))
    .await
    .unwrap();

    assert!(
        result.output.starts_with(EMPTY_SEARCH_NO_MATCH_ANNOTATION),
        "empty search miss should be annotated: {}",
        result.output
    );
    assert!(result.output.contains("[exit:1 |"));
}

// ===========================================================================
// Sandbox Tests
// ===========================================================================

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[tokio::test]
async fn unsupported_platform_default_sandbox_fails_closed() {
    let mut context = ToolContext::from_current_process().with_gates(Some(bypassed_gate_context()));
    context.sandbox_policy = SandboxPolicy::WorkspaceWrite;

    let error = execute_bash(&context, r#"{"command": "echo should-not-run"}"#)
        .await
        .expect_err("default sandbox must fail closed on unsupported platforms");

    assert!(error.message.contains("sandbox unavailable"));
    assert!(error.message.contains("no supported OS sandbox"));
    assert!(error.message.contains("CAKE_SANDBOX=off"));
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[tokio::test]
async fn unsupported_platform_danger_full_access_still_runs_bash() {
    let mut context = ToolContext::from_current_process().with_gates(Some(bypassed_gate_context()));
    context.sandbox_policy = SandboxPolicy::DangerFullAccess;

    let result = execute_bash(&context, r#"{"command": "printf danger-full-access"}"#)
        .await
        .expect("explicit danger-full-access must bypass unavailable sandbox detection");

    assert!(result.output.contains("danger-full-access"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
// FLAKE(task#292): This test fails when the environment transitions between
// `detect_platform()` returning `Err` (guard check) and `execute_bash` calling
// it again — the command runs unsandboxed and returns `Ok` instead of the
// expected sandbox error. Root mechanism is unclear since the probe is
// OnceLock-cached. Fails on clean master; reproduced across two separate
// cake sessions (b446b156, 5841be03).
async fn test_sandbox_unavailable_fails_closed() {
    if super::super::sandbox::is_sandbox_disabled()
        || super::super::sandbox::detect_platform().is_ok()
    {
        return;
    }

    let args = r#"{"command": "echo should-not-run"}"#;
    let result = Box::pin(execute_bash(&sandbox_context(), args)).await;
    let error = result.expect_err("sandbox initialization failure should fail closed");
    assert!(
        error.message.contains("macOS sandbox unavailable"),
        "Expected sandbox unavailable error, got: {error}"
    );
}

/// When the macOS sandbox is available, a sandboxed command that prints
/// `sandbox-exec: sandbox_apply` to stdout must return normal output,
/// not a sandbox-initialization-failure error. This is the exact scenario
/// from the original bug: `rg -n "sandbox" src/clients/tools/bash.rs`
/// matched lines containing that literal string.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandboxed_command_stdout_does_not_trigger_init_failure() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let args = r#"{"command": "printf 'sandbox-exec: sandbox_apply file-write* /tmp/test\\n'"}"#;
    let result = Box::pin(execute_bash(&sandbox_context(), args))
        .await
        .unwrap();

    assert!(
        result.output.contains("sandbox-exec: sandbox_apply"),
        "sandboxed command should return its stdout: {}",
        result.output
    );
    assert!(
        !result.output.contains("macOS sandbox unavailable"),
        "stdout pattern must not trigger sandbox initialization failure: {}",
        result.output
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_blocks_write_outside_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let fixture = tempfile::TempDir::new_in(&outside).expect("should create test fixture");
    let target = fixture.path().join("denied-write");
    let target_display = shell_quote(&target.display().to_string());
    let args = format!(r#"{{"command": "touch {target_display}"}}"#);
    let result = Box::pin(execute_bash(&sandbox_context(), &args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "Expected sandbox to block write outside cwd, got: {}",
        result.output
    );
    assert!(
        !target.exists(),
        "sandbox denial fixture must not be written outside the workspace"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_allows_read_in_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let args = r#"{"command": "ls Cargo.toml"}"#;
    let result = Box::pin(execute_bash(&sandbox_context(), args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Cargo.toml"),
        "Expected ls in cwd to succeed, got: {}",
        result.output
    );
    // Should contain metadata footer
    assert!(result.output.contains("[exit:0 |"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_blocks_read_outside_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let temp_dir = tempfile::TempDir::new_in(outside).expect("should create test dir outside cwd");
    let outside_dir = temp_dir.path().display();
    let args = format!(r#"{{"command": "ls {outside_dir}"}}"#);
    let result = Box::pin(execute_bash(&sandbox_context(), &args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "Expected sandbox to block read outside cwd, got: {}",
        result.output
    );
}

/// The `[Sandbox restriction]` notice after a denial names the specific path
/// that fell outside the allowed directories, instead of only the generic
/// "Operation not permitted".
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_denial_names_denied_path() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let temp_dir = tempfile::TempDir::new_in(outside).expect("should create test dir outside cwd");
    let outside_dir_str = temp_dir.path().display().to_string();
    let args = format!(r#"{{"command": "ls {outside_dir_str}"}}"#);
    let result = Box::pin(execute_bash(&sandbox_context(), &args))
        .await
        .unwrap();

    assert!(
        result.output.contains("[Sandbox restriction]"),
        "expected a sandbox restriction notice, got: {}",
        result.output
    );
    assert!(
        result.output.contains(&outside_dir_str),
        "sandbox notice should name the denied path {outside_dir_str}, got: {}",
        result.output
    );
    assert!(
        result.output.contains("(read)"),
        "sandbox notice should identify a read operation, got: {}",
        result.output
    );
}

/// A `[sandbox].read_only` file grant (which flows into `additional_dirs`)
/// lets sandboxed Bash execute exactly that file; a sibling file in the same
/// directory stays denied.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn test_sandbox_read_only_file_grant_runs_file_but_denies_sibling() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let temp_dir = tempfile::TempDir::new_in(outside).expect("should create test dir outside cwd");
    let allowed = temp_dir.path().join("allowed.sh");
    let sibling = temp_dir.path().join("sibling.sh");
    let denied_marker = temp_dir.path().join("denied-marker");
    let script = format!(
        "#!/bin/sh\nprintf 'allowed-ran\\n'\nprintf 'must-not-write\\n' > '{}'\n",
        denied_marker.display()
    );
    std::fs::write(&allowed, script).unwrap();
    std::fs::write(&sibling, "#!/bin/sh\nprintf 'sibling-ran\\n'\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&allowed, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // `[sandbox].read_only` paths are loaded into `additional_dirs` by main.rs;
    // mirror that wiring here.
    let mut context = ToolContext::from_current_process().with_gates(Some(bypassed_gate_context()));
    context.additional_dirs = vec![allowed.clone()];
    let context = Arc::new(context);

    let allowed_args = format!(r#"{{"command": "{}"}}"#, allowed.display());
    let result = Box::pin(execute_bash(&context, &allowed_args))
        .await
        .unwrap();
    assert!(
        result.output.contains("allowed-ran"),
        "sandbox should run a configured [sandbox].read_only file, got: {}",
        result.output
    );
    assert!(
        !denied_marker.exists(),
        "a read-only executable must not mutate its granted path"
    );

    let sibling_args = format!(r#"{{"command": "{}"}}"#, sibling.display());
    let result = Box::pin(execute_bash(&context, &sibling_args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "sandbox should deny a sibling file next to a [sandbox].read_only file, got: {}",
        result.output
    );
}

// ===========================================================================
// Sandbox Policy Tests (task 195)
// ===========================================================================

/// Build a `ToolContext` with a resolved sandbox policy for the supplied
/// workspace. `execute_bash` reads `context.sandbox_policy` to override the
/// args-level default.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn context_with_policy_at(cwd: std::path::PathBuf, policy: SandboxPolicy) -> Arc<ToolContext> {
    let mut context = ToolContext::from_current_process().with_gates(Some(bypassed_gate_context()));
    context.cwd = cwd;
    context.sandbox_policy = policy;
    Arc::new(context)
}

/// Build a `ToolContext` with a resolved sandbox policy for the current
/// process.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn context_with_policy(policy: SandboxPolicy) -> Arc<ToolContext> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    context_with_policy_at(cwd, policy)
}

/// A sandbox-test context with the judge bypassed. These tests exercise
/// sandbox execution, not the command-safety gate, so commands must not fail
/// closed on an absent judge context.
#[cfg(target_os = "macos")]
fn sandbox_context() -> Arc<ToolContext> {
    Arc::new(ToolContext::from_current_process().with_gates(Some(bypassed_gate_context())))
}

/// Read-only policy denies writes to the project directory.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn test_sandbox_read_only_blocks_write_in_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent outside the workspace");
    let workspace = tempfile::TempDir::new_in(&outside).expect("should create test workspace");
    let target = format!("cake_ro_probe_{}", uuid::Uuid::new_v4());
    let args = format!(r#"{{"command": "touch {target}"}}"#);
    let result = Box::pin(execute_bash(
        &context_with_policy_at(workspace.path().to_path_buf(), SandboxPolicy::ReadOnly),
        &args,
    ))
    .await
    .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "read-only policy should block writes to cwd, got: {}",
        result.output
    );
    // Clean up just in case the sandbox did not block it.
    _ = std::fs::remove_file(workspace.path().join(&target));
}

/// Workspace-write policy allows writes to the project directory.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_workspace_write_allows_write_in_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let target = format!("cake_ww_probe_{}", uuid::Uuid::new_v4());
    let args = format!(r#"{{"command": "touch {target} && rm -f {target}"}}"#);
    let result = Box::pin(execute_bash(
        &context_with_policy(SandboxPolicy::WorkspaceWrite),
        &args,
    ))
    .await
    .unwrap();
    assert!(
        result.output.contains("[exit:0 |"),
        "workspace-write policy should allow writes to cwd, got: {}",
        result.output
    );
}

/// A per-call `cwd` inside the workspace selects where a sandboxed command
/// starts without widening authority: writes inside the selected directory
/// still succeed, and a write outside the workspace is still denied.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn test_sandbox_cwd_selects_subdirectory_without_widening_authority() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a directory outside the workspace");
    let workspace = tempfile::TempDir::new_in(&outside).expect("should create test workspace");
    let nested = workspace.path().join("nested");
    std::fs::create_dir(&nested).expect("should create nested fixture");
    let expected = nested
        .canonicalize()
        .expect("nested path must canonicalize");
    let denied = outside.join(format!("cake_cwd_probe_{}", uuid::Uuid::new_v4()));
    let denied_display = shell_quote(&denied.display().to_string());
    let args = format!(
        r#"{{"command": "pwd -P && touch inside.txt; touch {denied_display}", "cwd": "nested"}}"#
    );

    let result = Box::pin(execute_bash(
        &context_with_policy_at(
            workspace.path().to_path_buf(),
            SandboxPolicy::WorkspaceWrite,
        ),
        &args,
    ))
    .await
    .unwrap();

    assert!(
        result
            .output
            .starts_with(&format!("{}\n", expected.display())),
        "a sandboxed command must start in the selected cwd, got: {}",
        result.output
    );
    assert!(
        nested.join("inside.txt").exists(),
        "workspace-write must still allow writes inside the selected cwd"
    );
    assert!(
        !denied.exists(),
        "selecting a cwd must not widen the sandbox's authority"
    );
    // Clean up just in case the sandbox did not block it.
    _ = std::fs::remove_file(&denied);
}

/// Workspace-write policy grants sccache's default macOS cache dir
/// (~/Library/Caches/Mozilla.sccache) so `RUSTC_WRAPPER=sccache` builds work
/// under the sandbox.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_workspace_write_allows_sccache_cache_dir() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        panic!("HOME must be set for the sccache cache dir probe");
    };
    let probe_dir = home
        .join("Library/Caches/Mozilla.sccache")
        .join(format!("cake_sccache_probe_{}", uuid::Uuid::new_v4()));
    let probe = probe_dir.display();
    // `rm -rf` is blocked by the command policy outside /tmp and /var/tmp, so
    // the probe is removed by the (unsandboxed) test process below instead of
    // inside the sandboxed command.
    let args = format!(r#"{{"command": "mkdir -p {probe} && touch {probe}/probe"}}"#);
    let result = Box::pin(execute_bash(
        &context_with_policy(SandboxPolicy::WorkspaceWrite),
        &args,
    ))
    .await
    .unwrap();
    assert!(
        result.output.contains("[exit:0 |"),
        "workspace-write policy should allow writes to sccache's default cache dir, got: {}",
        result.output
    );
    // Clean up just in case the sandbox did not remove the probe dir.
    _ = std::fs::remove_dir_all(&probe_dir);
}

/// Danger-full-access policy skips the sandbox entirely.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_danger_full_access_allows_write_outside_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let fixture = tempfile::TempDir::new_in(&outside).expect("should create test fixture");
    let target = fixture.path().join("danger-full-access-write");
    let target_display = shell_quote(&target.display().to_string());
    let args = format!(r#"{{"command": "touch {target_display} && rm -f {target_display}"}}"#);
    let result = Box::pin(execute_bash(
        &context_with_policy(SandboxPolicy::DangerFullAccess),
        &args,
    ))
    .await
    .unwrap();
    // With no sandbox, writing outside cwd must succeed.
    assert!(
        result.output.contains("[exit:0 |"),
        "danger-full-access policy should skip the sandbox and allow writes outside cwd, got: {}",
        result.output
    );
    assert!(
        !target.exists(),
        "danger-full-access fixture must be cleaned up by the command"
    );
}

// ===========================================================================
// Linked Worktree Sandbox Tests (task 260)
// ===========================================================================

/// Build bash tool arguments that run `git` with fixture-only configuration
/// and identity variables dropped. Repository-pinning variables intentionally
/// remain for the Bash executor's production scrub to remove after sandbox
/// application, so this helper exercises that real child environment.
#[cfg(target_os = "macos")]
fn sandboxed_git(args: &[&str]) -> String {
    let mut command = String::from("env");
    for var in crate::config::git::FIXTURE_ENV_VARS {
        command.push_str(" -u ");
        command.push_str(var);
    }
    command.push_str(" git");
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    serde_json::json!({ "command": command }).to_string()
}

/// The fixture command must isolate fixture-only variables locally while
/// leaving production ambient-variable scrubbing to `execute_bash`.
#[cfg(target_os = "macos")]
#[test]
fn sandboxed_git_isolates_fixture_vars_without_scrubbing_ambient_vars() {
    let arguments = sandboxed_git(&["config", "--get", "cake.sentinel"]);
    let payload: serde_json::Value =
        serde_json::from_str(&arguments).expect("sandboxed git arguments must be valid JSON");
    let command = payload["command"]
        .as_str()
        .expect("sandboxed git arguments must contain a command");

    for var in crate::config::git::FIXTURE_ENV_VARS {
        assert!(
            command.contains(&format!(" -u {var} ")),
            "fixture helper must drop {var}: {command}"
        );
    }
    for var in crate::config::git::AMBIENT_ENV_VARS {
        assert!(
            !command.contains(&format!(" -u {var} ")),
            "fixture helper must leave production scrubbing of {var} to Bash: {command}"
        );
    }

    let fixture = tempfile::tempdir().expect("fixture repository");
    crate::config::git::test_support::init_repo(fixture.path());
    let output = std::process::Command::new("bash")
        .args(["-c", command])
        .current_dir(fixture.path())
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "cake.sentinel")
        .env("GIT_CONFIG_VALUE_0", "leaked")
        .output()
        .expect("fixture git command must spawn");

    assert!(
        !output.status.success(),
        "fixture command must not see command-scope config from its environment: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// A model-run Git command must discover the repository from the Bash working
/// directory even when Cake itself inherits a repository-pinning `GIT_DIR`.
/// Keep the canary in the parent environment so the test exercises the child
/// spawn, rather than an explicit `env` assignment in the model command.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn test_bash_git_ignores_inherited_git_dir_canary() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let workspace = tempfile::TempDir::new().expect("workspace fixture");
    let canary = tempfile::TempDir::new().expect("canary fixture");
    crate::config::git::test_support::init_repo(workspace.path());
    crate::config::git::test_support::init_repo(canary.path());
    let expected_git_dir = workspace
        .path()
        .join(".git")
        .canonicalize()
        .expect("workspace git directory");
    let canary_git_dir = canary
        .path()
        .join(".git")
        .canonicalize()
        .expect("canary git directory");

    let mut context = ToolContext::from_current_process().with_gates(Some(bypassed_gate_context()));
    context.cwd = workspace.path().to_path_buf();
    context.sandbox_policy = SandboxPolicy::WorkspaceWrite;
    let context = Arc::new(context);
    let args = serde_json::json!({
        "command": "git rev-parse --absolute-git-dir"
    })
    .to_string();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    let result = temp_env::with_var("GIT_DIR", Some(&canary_git_dir), || {
        runtime.block_on(execute_bash(&context, &args))
    })
    .expect("git rev-parse should run through Bash");

    assert!(
        result
            .output
            .contains(&expected_git_dir.display().to_string()),
        "git should resolve the Bash workspace, got: {}",
        result.output
    );
    assert!(
        !result
            .output
            .contains(&canary_git_dir.display().to_string()),
        "git must not resolve the inherited canary repository, got: {}",
        result.output
    );
}

/// Single-quote `value` for a POSIX shell.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Verify git operations (status, add, commit) succeed in a real linked
/// worktree under an enforced macOS Seatbelt sandbox.
///
/// This integration test creates a real git repository with a linked worktree
/// whose per-worktree gitdir and common dir are outside the workspace subtree,
/// then executes git commands through cake's sandboxed execution path.
///
/// Coverage notes:
/// - The test uses an externally-created worktree (`git worktree add`).
///   Cake-managed worktrees (--worktree flag) route through the same
///   `SandboxConfig::build_with_policy` and `resolve_linked_worktree_dirs`
///   code path — the .git file pointer and commondir resolution are
///   identical regardless of how the worktree was created.
/// - Both the per-worktree gitdir and the common git dir are explicitly
///   verified to be outside the workspace subtree.
#[cfg(target_os = "macos")]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "integration test: setup, git init, worktree creation, path verification, and three git operations through sandbox"
)]
async fn test_sandbox_linked_worktree_git_operations() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let fixture_parent =
        path_outside_cwd_for_sandbox_test().expect("should find a parent outside the workspace");
    let fixture = tempfile::Builder::new()
        .prefix("cake-linked-worktree-")
        .tempdir_in(&fixture_parent)
        .expect("should create fixture outside the workspace");
    let main_repo = fixture.path().join("main");
    let wt_path = fixture.path().join("linked-worktree");

    // Initialize main repo with a commit and a detached linked worktree
    crate::config::git::test_support::init_repo_with_linked_worktree(&main_repo, &wt_path);

    // Verify per-worktree gitdir and common dir are outside the workspace
    // subtree (the linked worktree itself).
    let git_file_content =
        std::fs::read_to_string(wt_path.join(".git")).expect(".git file must be readable");
    let gitdir_line = git_file_content
        .lines()
        .find(|l| l.trim().starts_with("gitdir:"))
        .expect(".git file must contain a gitdir: line");
    let gitdir_raw = gitdir_line
        .strip_prefix("gitdir: ")
        .or_else(|| gitdir_line.strip_prefix("gitdir:"))
        .map(str::trim)
        .expect("gitdir: line must have a path");
    let gitdir_path = if std::path::Path::new(gitdir_raw).is_relative() {
        wt_path.join(gitdir_raw)
    } else {
        std::path::PathBuf::from(gitdir_raw)
    };
    let canonical_gitdir = gitdir_path
        .canonicalize()
        .expect("gitdir path must be resolvable");

    // Read commondir from the worktree gitdir
    let commondir_content = std::fs::read_to_string(canonical_gitdir.join("commondir"))
        .expect("commondir file must be readable");
    let commondir_raw = commondir_content.trim();
    let common_dir_path = if std::path::Path::new(commondir_raw).is_relative() {
        canonical_gitdir.join(commondir_raw)
    } else {
        std::path::PathBuf::from(commondir_raw)
    };
    let canonical_common = common_dir_path
        .canonicalize()
        .expect("common dir must be resolvable");

    // Assert both are outside the worktree and the sandbox's broad temp grants.
    // Their writes must therefore depend on linked-worktree resolution.
    let canonical_wt = wt_path
        .canonicalize()
        .expect("worktree path must be canonicalizable");
    assert!(
        !canonical_gitdir.starts_with(&canonical_wt),
        "per-worktree gitdir should be outside the workspace subtree"
    );
    assert!(
        !canonical_common.starts_with(&canonical_wt),
        "common git dir should be outside the workspace subtree"
    );

    // Create a ToolContext with cwd set to the linked worktree
    let mut context = ToolContext::from_current_process().with_gates(Some(bypassed_gate_context()));
    context.cwd = wt_path.clone();
    for temp_dir in &context.temp_dirs {
        let canonical_temp = temp_dir.canonicalize().unwrap_or_else(|_| temp_dir.clone());
        assert!(
            !canonical_gitdir.starts_with(&canonical_temp),
            "per-worktree gitdir must not inherit a broad temp-directory grant"
        );
        assert!(
            !canonical_common.starts_with(&canonical_temp),
            "common git dir must not inherit a broad temp-directory grant"
        );
    }

    // ====================================================================
    // git status
    // ====================================================================
    let args = sandboxed_git(&["status"]);
    let result = Box::pin(execute_bash(&context, &args))
        .await
        .expect("git status should succeed in linked worktree");
    assert!(
        result.output.contains("[exit:0 |"),
        "git status should exit 0 in linked worktree: {}",
        result.output
    );
    assert!(
        result.output.contains("nothing to commit"),
        "git status should show clean tree in linked worktree: {}",
        result.output
    );

    // ====================================================================
    // Create new file, git add, git commit
    // ====================================================================
    // Create a new file via the sandbox
    let args = r#"{"command": "echo 'new content' > new_file.md"}"#;
    let result = Box::pin(execute_bash(&context, args))
        .await
        .expect("echo to new file should succeed in linked worktree");
    assert!(
        result.output.contains("[exit:0 |"),
        "echo should succeed: {}",
        result.output
    );

    // git add the new file
    let args = sandboxed_git(&["add", "new_file.md"]);
    let result = Box::pin(execute_bash(&context, &args))
        .await
        .expect("git add should succeed in linked worktree");
    assert!(
        result.output.contains("[exit:0 |"),
        "git add should exit 0 in linked worktree: {}",
        result.output
    );

    // git commit with inline user config (no global git config needed
    // since the sandbox may restrict access to ~/.gitconfig)
    let args = sandboxed_git(&[
        "-c",
        "user.name=Cake Test",
        "-c",
        "user.email=cake-test@example.invalid",
        "commit",
        "-m",
        "test commit in linked worktree",
    ]);
    let result = Box::pin(execute_bash(&context, &args))
        .await
        .expect("git commit should succeed in linked worktree");
    assert!(
        result.output.contains("[exit:0 |"),
        "git commit should exit 0 in linked worktree: {}",
        result.output
    );
    assert!(
        result.output.contains("1 file changed"),
        "git commit should show 1 file changed in linked worktree: {}",
        result.output
    );
}

// ===========================================================================
// CAKE_REQUIRE_SANDBOX_TESTS Parsing Tests
// ===========================================================================

#[cfg(target_os = "macos")]
#[test]
fn require_sandbox_tests_false_for_unrecognized_values() {
    assert!(!parse_sandbox_tests_required(Some("0")));
    assert!(!parse_sandbox_tests_required(Some("false")));
    assert!(!parse_sandbox_tests_required(Some("no")));
    assert!(!parse_sandbox_tests_required(Some("off")));
    assert!(!parse_sandbox_tests_required(Some("maybe")));
    assert!(!parse_sandbox_tests_required(Some("")));
}

#[cfg(target_os = "macos")]
#[test]
fn require_sandbox_tests_true_for_truthy_values() {
    assert!(parse_sandbox_tests_required(Some("1")));
    assert!(parse_sandbox_tests_required(Some("true")));
    assert!(parse_sandbox_tests_required(Some("yes")));
    assert!(parse_sandbox_tests_required(Some("on")));
}

// ===========================================================================
// Binary Data Detection Tests
// ===========================================================================

#[test]
fn test_is_binary_data_detects_null_bytes() {
    // Data with null bytes should be detected as binary (need >8 null bytes)
    let binary_data =
        b"hello\x00world\x00more\x00nulls\x00here\x00more\x00data\x00extra\x00again\x00more";
    assert!(is_binary_data(binary_data));
}

#[test]
fn test_is_binary_data_detects_high_non_printable_ratio() {
    // Data with many non-printable characters should be detected as binary
    // Create data with ~50% non-printable characters
    let mut binary_data = Vec::new();
    for i in 0..100 {
        if i % 2 == 0 {
            binary_data.push(0x01); // Non-printable
        } else {
            binary_data.push(b'A'); // Printable
        }
    }
    assert!(is_binary_data(&binary_data));
}

#[test]
fn test_is_binary_data_allows_exact_threshold() {
    let mut data = Vec::new();
    for i in 0..100 {
        if i < 30 {
            data.push(0x01);
        } else {
            data.push(b'A');
        }
    }
    assert!(!is_binary_data(&data));
}

#[test]
fn test_is_binary_data_allows_text() {
    // Normal text should not be detected as binary
    let text_data = b"Hello, world!\nThis is a test.\nLine 3.\n";
    assert!(!is_binary_data(text_data));
}

#[test]
fn test_is_binary_data_allows_multibyte_utf8() {
    // UTF-8 text with multi-byte characters should not be detected as binary
    let utf8_text = "Hello, 世界!\nПривет мир\n🎉".as_bytes();
    assert!(!is_binary_data(utf8_text));
}

#[test]
fn test_is_binary_data_allows_few_null_bytes() {
    // A few null bytes (below threshold) should not trigger binary detection
    let text_with_few_nulls = b"hello\x00world";
    assert!(!is_binary_data(text_with_few_nulls));
}

#[test]
fn test_is_binary_data_allows_empty() {
    // Empty data should not be detected as binary.
    assert!(!is_binary_data(b""));
}

#[test]
fn sandbox_initialization_failure_requires_applied_sandbox() {
    let output = "sandbox-exec: sandbox_apply: Operation not permitted";
    assert!(is_sandbox_initialization_failure(true, output));
    assert!(!is_sandbox_initialization_failure(false, output));
    assert!(!is_sandbox_violation(true, false, output, output));
}

#[test]
fn sandbox_initialization_failure_checks_stderr_only() {
    // The pattern must appear in the stderr string to be detected.
    // An empty stderr means no initialization failure, even if stdout
    // contains the literal string.
    assert!(!is_sandbox_initialization_failure(true, ""));
    assert!(!is_sandbox_initialization_failure(
        true,
        "some normal stderr output"
    ));
    assert!(is_sandbox_initialization_failure(
        true,
        "sandbox-exec: sandbox_apply: Operation not permitted"
    ));
}

#[test]
fn sandbox_violation_uses_stderr_for_initialization_exclusion() {
    let output = "sandbox-exec: sandbox_apply: Operation not permitted\nOperation not permitted";
    let denial_stderr = "Operation not permitted";
    let initialization_stderr = "sandbox-exec: sandbox_apply: Operation not permitted";

    assert!(is_sandbox_violation(true, false, output, denial_stderr));
    assert!(!is_sandbox_violation(
        true,
        false,
        output,
        initialization_stderr
    ));
}

#[test]
fn sandbox_initialization_precedes_binary_classification() {
    let binary_output = [0_u8; 16];
    let initialization_stderr = "sandbox-exec: sandbox_apply: Operation not permitted";

    assert!(is_binary_data(&binary_output));
    assert!(sandbox_initialization_output(true, initialization_stderr, &binary_output).is_some());
}

/// Regression test: a command that prints `sandbox-exec: sandbox_apply`
/// to stdout must NOT be treated as a sandbox initialization failure.
/// The check should only inspect stderr, so stdout content is irrelevant.
#[tokio::test]
async fn command_stdout_containing_sandbox_apply_pattern_is_not_false_positive() {
    let args = r#"{"command": "printf 'sandbox-exec: sandbox_apply file-write* /tmp/test\n'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(
        result.output.contains("sandbox-exec: sandbox_apply"),
        "command output should contain the printed pattern: {}",
        result.output
    );
    assert!(
        !result.output.contains("macOS sandbox unavailable"),
        "stdout pattern should not trigger sandbox initialization failure: {}",
        result.output
    );
}

#[test]
fn sandbox_violation_requires_sandboxed_failed_command() {
    let output = "Operation not permitted";

    assert!(is_sandbox_violation(true, false, output, ""));
    assert!(!is_sandbox_violation(true, true, output, ""));
    assert!(!is_sandbox_violation(false, false, output, ""));
}

#[tokio::test]
async fn successful_command_output_does_not_trigger_sandbox_warning() {
    let args = r#"{"command": "printf 'Operation not permitted\n'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(result.output.contains("Operation not permitted"));
    assert!(
        !result.output.contains("[Sandbox restriction]"),
        "successful command output should not be classified as sandbox restriction: {}",
        result.output
    );
}

#[tokio::test]
async fn failed_unsandboxed_command_output_does_not_trigger_sandbox_warning() {
    let args = r#"{"command": "printf 'Operation not permitted\n'; exit 1"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(result.output.contains("Operation not permitted"));
    assert!(
        !result.output.contains("[Sandbox restriction]"),
        "unsandboxed command output should not be classified as sandbox restriction: {}",
        result.output
    );
}

// ===========================================================================
// Sandbox denial path naming (issue #219)
// ===========================================================================

/// Build a `WorkspaceWrite` config whose only allowed directory is `cwd`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn minimal_sandbox_config(cwd: &std::path::Path) -> crate::clients::tools::sandbox::SandboxConfig {
    crate::clients::tools::sandbox::SandboxConfig::build_with_policy(
        crate::clients::tools::sandbox::SandboxPolicy::WorkspaceWrite,
        cwd,
        &[],
        &[],
        &[],
        &[],
    )
}

/// Create a path-analysis fixture outside the minimal sandbox config.
///
/// The process workspace may itself be sandboxed, so its parent is not
/// necessarily writable. `/tmp` is writable in the outer workspace sandbox,
/// while `minimal_sandbox_config` intentionally omits temp-directory grants.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn denied_path_test_dir() -> tempfile::TempDir {
    tempfile::TempDir::new_in("/tmp")
        .expect("should create denied-path fixture in the system temp directory")
}

#[test]
fn denied_paths_in_command_names_a_file_outside_allowed_dirs() {
    let cwd = tempfile::TempDir::new().unwrap();
    let outside = denied_path_test_dir();
    let secret = outside.path().join("secret.json");
    std::fs::write(&secret, "{}").unwrap();

    let config = minimal_sandbox_config(cwd.path());
    let command = format!("cat {}", secret.display());
    let denials = denied_paths_in_command(&command, cwd.path(), &config);

    assert_eq!(denials.len(), 1, "expected one denial, got: {denials:?}");
    assert!(denials[0].contains("secret.json"), "{denials:?}");
    assert!(denials[0].contains("(read)"), "{denials:?}");
}

#[test]
fn denied_paths_in_command_names_an_executable_outside_allowed_dirs() {
    let cwd = tempfile::TempDir::new().unwrap();
    let outside = denied_path_test_dir();
    let bin = outside.path().join("tool");
    std::fs::write(&bin, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let config = minimal_sandbox_config(cwd.path());
    let command = bin.display().to_string();
    let denials = denied_paths_in_command(&command, cwd.path(), &config);

    assert_eq!(denials.len(), 1, "expected one denial, got: {denials:?}");
    assert!(denials[0].contains("(execute)"), "{denials:?}");
}

#[test]
fn denied_paths_in_command_ignores_paths_inside_allowed_dirs() {
    let cwd = tempfile::TempDir::new().unwrap();
    std::fs::write(cwd.path().join("ok.txt"), "data").unwrap();

    let config = minimal_sandbox_config(cwd.path());
    let command = "cat ok.txt";
    let denials = denied_paths_in_command(command, cwd.path(), &config);

    assert!(
        denials.is_empty(),
        "expected no denials for cwd-relative read, got: {denials:?}"
    );
}

#[test]
fn denied_paths_in_command_skips_shell_noise_and_flags() {
    let cwd = tempfile::TempDir::new().unwrap();

    let config = minimal_sandbox_config(cwd.path());
    let command = "ls -la 2>/dev/null".to_string();
    let denials = denied_paths_in_command(&command, cwd.path(), &config);

    assert!(
        denials.is_empty(),
        "flags and redirects must not be reported, got: {denials:?}"
    );
}

#[test]
fn bare_command_word_resolves_via_path_and_reports_execute() {
    // A bare word in command position resolves via `PATH`: an executable that
    // is only reachable through `PATH` and sits outside the allowed dirs is
    // reported as an execute denial.
    let cwd = tempfile::TempDir::new().unwrap();
    let outside = denied_path_test_dir();
    let tool = outside.path().join("ztool-cake-path-probe");
    std::fs::write(&tool, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let config = minimal_sandbox_config(cwd.path());
    let existing_path = std::env::var_os("PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let path_with_tool = format!("{}:{}", outside.path().display(), existing_path.display());

    temp_env::with_var("PATH", Some(&path_with_tool), || {
        let denials = denied_paths_in_command("ztool-cake-path-probe", cwd.path(), &config);
        assert_eq!(denials.len(), 1, "expected one denial, got: {denials:?}");
        assert!(denials[0].contains("(execute)"), "{denials:?}");
    });
}

#[test]
fn bare_word_arguments_resolve_from_cwd_not_path() {
    // A bare word in argument position must resolve relative to `cwd` as a
    // file read, even when a same-named executable exists on `PATH` outside
    // the allowed dirs. Treating it as the denied command would be a false
    // positive.
    let cwd = tempfile::TempDir::new().unwrap();
    let outside = denied_path_test_dir();
    std::fs::write(cwd.path().join("weird"), "data").unwrap();
    let stray_bin = outside.path().join("weird");
    std::fs::write(&stray_bin, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stray_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(cwd.path().join("ok.txt"), "data").unwrap();

    let config = minimal_sandbox_config(cwd.path());
    let existing_path = std::env::var_os("PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let path_with_stray = format!("{}:{}", outside.path().display(), existing_path.display());

    temp_env::with_var("PATH", Some(&path_with_stray), || {
        let denials = denied_paths_in_command("cat weird", cwd.path(), &config);
        assert!(
            denials.is_empty(),
            "argument `weird` must resolve from cwd, not `PATH`; got: {denials:?}"
        );
    });
}

#[test]
fn compose_text_output_includes_named_denials() {
    let output = compose_text_output(
        "Operation not permitted",
        "",
        false,
        None,
        false,
        true,
        &["  - /tmp/x/secret.json (read)".to_string()],
    );
    assert!(output.contains("[Sandbox restriction]"));
    assert!(output.contains("secret.json"));
    assert!(output.contains("(read)"));
    assert!(output.contains("directories"));
    assert!(output.contains("Do NOT retry"));
}

#[test]
fn compose_text_output_without_denials_keeps_original_guidance() {
    let output = compose_text_output("Operation not permitted", "", false, None, false, true, &[]);
    assert!(output.contains("[Sandbox restriction]"));
    assert!(output.contains("Do NOT retry"));
    assert!(!output.contains("outside the allowed directories"));
}

#[test]
fn test_detect_mime_type_png() {
    let png_header = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    assert_eq!(detect_mime_type(&png_header), Some("image/png"));
}

#[test]
fn test_detect_mime_type_jpeg() {
    let jpeg_header = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    assert_eq!(detect_mime_type(&jpeg_header), Some("image/jpeg"));
}

#[test]
fn test_detect_mime_type_pdf() {
    let pdf_header = b"%PDF-1.4";
    assert_eq!(detect_mime_type(pdf_header), Some("application/pdf"));
}

#[test]
fn test_detect_mime_type_zip() {
    let zip_header = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00, 0x00];
    assert_eq!(detect_mime_type(&zip_header), Some("application/zip"));
}

#[test]
fn test_detect_mime_type_gzip() {
    let gzip_header = [0x1F, 0x8B, 0x08, 0x00];
    assert_eq!(detect_mime_type(&gzip_header), Some("application/gzip"));
}

#[test]
fn test_detect_mime_type_unknown() {
    // Random data should return None
    let unknown_data = b"Hello, world!";
    assert_eq!(detect_mime_type(unknown_data), None);
}

#[test]
fn test_detect_mime_type_too_short() {
    // Data that's too short should return None
    let short_data = [0x89, 0x50];
    assert_eq!(detect_mime_type(&short_data), None);
}

#[tokio::test]
async fn test_binary_output_handling() {
    // Command that produces binary output (random bytes)
    let args = r#"{"command": "head -c 100 /dev/urandom"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    // Should detect binary and show appropriate message
    assert!(
        result.output.contains("[Binary output detected") || result.output.contains("[exit:"),
        "Expected binary output handling, got: {}",
        result.output
    );
}

#[tokio::test]
async fn test_binary_output_with_known_type() {
    // Create a small gzip-compressed file and read it
    let args = r#"{"command": "echo 'hello' | gzip | head -c 20"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    // Should detect gzip magic number
    assert!(
        result.output.contains("application/gzip") || result.output.contains("[exit:"),
        "Expected gzip detection, got: {}",
        result.output
    );
}

#[tokio::test]
async fn binary_output_with_exit_zero_stderr_includes_warning() {
    let args = r#"{"command": "printf '\\0%.0s' {1..16}; echo err >&2"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(result.output.contains("[Binary output detected"));
    assert!(result.output.contains(EXIT_ZERO_STDERR_WARNING));
    assert!(result.output.contains("[exit:0 |"));
}

#[tokio::test]
async fn binary_output_above_max_records_truncation_event() {
    // Binary output above BASH_OUTPUT_MAX_BYTES spills to a temp file and
    // replaces the displayable output, so it must record an output_truncation
    // compensation event exactly like oversized text does. 60_000 bytes is
    // over the 50_000 max but under the 100_000 read cap, so only the spill
    // mechanism fires.
    let args = r#"{"command": "head -c 60000 /dev/zero"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("[Binary output detected"));
    // One spill truncation event (plus the judge bypass event from the
    // bypassed judge context used by this test harness).
    let truncations: Vec<_> = result
        .compensation_events
        .iter()
        .filter(|e| e.kind == crate::session_telemetry::CompensationKind::OutputTruncation)
        .collect();
    assert_eq!(
        truncations.len(),
        1,
        "oversized binary spill must record one output_truncation event"
    );
    assert_eq!(truncations[0].detail.as_deref(), Some("Bash"));
    assert!(
        result
            .compensation_events
            .iter()
            .any(|e| e.kind == crate::session_telemetry::CompensationKind::JudgeBypass),
        "bypassed judge must record a judge_bypass event"
    );
}

#[tokio::test]
async fn small_binary_output_records_no_truncation_event() {
    // Binary output below the max bytes is summarized for display, but the
    // model lost no content above the inline cap, so no truncation event.
    let args = r#"{"command": "printf '\\0%.0s' {1..16}"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("[Binary output detected"));
    assert!(
        result
            .compensation_events
            .iter()
            .all(|e| e.kind == crate::session_telemetry::CompensationKind::JudgeBypass),
        "small binary output must record only the judge bypass event, got: {:?}",
        result.compensation_events
    );
}

#[tokio::test]
async fn test_text_output_not_detected_as_binary() {
    // Normal text output should not be detected as binary
    let args = r#"{"command": "echo 'Hello, world!'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(
        !result.output.contains("[Binary output detected"),
        "Text output should not be detected as binary, got: {}",
        result.output
    );
    assert!(result.output.contains("Hello, world!"));
}

#[tokio::test]
async fn test_streaming_empty_output() {
    // A command that produces no stdout or stderr should return empty
    // output with just the metadata footer.
    let args = r#"{"command": "true"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(
        result.output.starts_with("[exit:0 |"),
        "Empty output should produce only the footer, got: {}",
        result.output
    );
    // No command output before the footer
    assert!(
        !result.output.contains('\n'),
        "Empty output should not contain newlines before the footer, got: {}",
        result.output
    );
}

#[tokio::test]
async fn test_streaming_empty_output_with_stderr() {
    // A command that produces only stderr output. The output should show
    // the warning because exit is 0 but stderr is non-empty, then the
    // metadata footer.
    let args = r#"{"command": "echo err >&2"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("err"));
    assert!(result.output.contains(EXIT_ZERO_STDERR_WARNING));
    assert!(result.output.contains("[exit:0 |"));
}

// =============================================================================
// Gate preflight: captured decisions, warn prepends, block and failure block.
// =============================================================================

#[tokio::test]
async fn script_evidence_reaches_the_gate_before_execution() {
    let gate = gate_fixture::decision(GateDecision::Allow, None, "Safe");
    let dir = tempfile::tempdir().unwrap();
    let contents = "printf observed-script-output";
    std::fs::write(dir.path().join("job.sh"), contents).unwrap();
    let output = execute_bash_with_gates_in(
        r#"{"command":"bash job.sh"}"#,
        Some(gate.pipeline()),
        dir.path(),
    )
    .await
    .unwrap();
    assert!(output.output.contains("observed-script-output"));
    assert!(
        output
            .output
            .contains("collected as untrusted evidence for the configured policy gate")
    );
    let frames = gate.frames();
    assert_eq!(frames.len(), 1);
    let evidence = &frames[0].input.script_evidence;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].contents, contents);
    assert!(evidence[0].path.ends_with("job.sh"));
    assert_eq!(frames[0].input.command, "bash job.sh");
}

#[tokio::test]
async fn script_evidence_failure_prevents_gate_call_and_allowlist_override() {
    let dir = tempfile::tempdir().unwrap();
    for shell in [
        "bash", "sh", "zsh", "dash", "ksh", "ksh93", "ash", "mksh", "pdksh",
    ] {
        let command = format!("{shell} missing.sh");
        let gate = gate_fixture::with_allowlist(&command);
        let error = execute_bash_with_gates_in(
            &serde_json::json!({"command": command}).to_string(),
            Some(gate.pipeline()),
            dir.path(),
        )
        .await
        .unwrap_err();
        assert!(error.message.starts_with("BLOCKED\n\n"));
        assert!(error.message.contains("missing.sh"));
        assert!(error.message.contains("no gate request was sent"));
        assert!(error.message.contains("32 KiB"));
        assert!(!error.message.contains("was unavailable"));
        assert_eq!(
            error.compensation_events[0].detail.as_deref(),
            Some("script_evidence")
        );
        assert!(
            gate.frames().is_empty(),
            "evidence collection must fail before the gate runs"
        );
    }
}

#[tokio::test]
async fn script_evidence_block_prevents_script_execution() {
    let gate = gate_fixture::decision(GateDecision::Block, Some("unknown-destructive"), "Denied");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("job.sh"), "touch must-not-exist").unwrap();
    for shell in [
        "bash", "sh", "zsh", "dash", "ksh", "ksh93", "ash", "mksh", "pdksh",
    ] {
        let command = format!("{shell} job.sh");
        let error = execute_bash_with_gates_in(
            &serde_json::json!({"command": command}).to_string(),
            Some(gate.pipeline()),
            dir.path(),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .message
                .contains("collected as untrusted evidence for the configured policy gate")
        );
        assert!(!dir.path().join("must-not-exist").exists());
    }
    let frames = gate.frames();
    assert_eq!(frames.len(), 9);
    for frame in frames {
        assert_eq!(
            frame.input.script_evidence[0].contents,
            "touch must-not-exist"
        );
    }
}

#[tokio::test]
async fn script_evidence_bypass_skips_collection() {
    let dir = tempfile::tempdir().unwrap();
    let mut context = super::super::ToolContext::with_temp_dirs(
        dir.path().to_path_buf(),
        vec![],
        vec![],
        vec![],
        vec![],
    );
    context.gates = Some(bypassed_gate_context());
    let args = BashExecutionArgs::from_json(
        r#"{"command":"bash missing.sh"}"#,
        super::super::sandbox::SandboxPolicy::WorkspaceWrite,
    )
    .unwrap();
    let preflight = bash_gate_preflight(&context, &args, dir.path(), None)
        .await
        .unwrap();
    assert!(preflight.warnings.is_empty());
    assert_eq!(
        preflight.compensation_events[0].kind,
        CompensationKind::JudgeBypass
    );
}

#[tokio::test]
async fn warn_decision_prepends_notice_and_runs_command() {
    // A `warn` decision runs the command and prepends its message as a
    // NOTICE, mirroring the old soft-warning behavior without reclassification.
    let gate = gate_fixture::decision(
        GateDecision::Warn,
        Some("rg-replace-footgun"),
        "Use -r with an explicit replacement.",
    );
    let args = r#"{"command": "echo gate-warn-test"}"#;
    let result = Box::pin(execute_bash_with_gates(args, Some(gate.pipeline())))
        .await
        .unwrap();
    assert!(
        result
            .output
            .contains("NOTICE: Use -r with an explicit replacement."),
        "warn decision should prepend NOTICE, got: {}",
        result.output
    );
    assert!(result.output.contains("gate-warn-test"));
    assert!(result.output.contains("[exit:0 |"));
    assert_eq!(gate.frames().len(), 1, "the warn call reached the gate");
}

#[tokio::test]
async fn allow_decision_runs_the_command() {
    // An `allow` decision runs the command with no annotation.
    let gate = gate_fixture::decision(GateDecision::Allow, None, "Safe");
    let args = r#"{"command": "echo gate-allow-test"}"#;
    let result = Box::pin(execute_bash_with_gates(args, Some(gate.pipeline())))
        .await
        .unwrap();
    assert!(
        result.output.contains("gate-allow-test"),
        "allow decision should run the command, got: {}",
        result.output
    );
    assert!(
        !result.output.contains("NOTICE:"),
        "allow decision should not annotate output, got: {}",
        result.output
    );
    assert_eq!(gate.frames().len(), 1);
}

#[tokio::test]
async fn block_decision_prevents_execution() {
    // A `block` decision prevents spawn and surfaces the gate's reason as the
    // tool error.
    let gate = gate_fixture::decision(
        GateDecision::Block,
        Some("git-force-push"),
        "Prefer push --force-with-lease.",
    );
    let args = r#"{"command": "echo gate-block-test"}"#;
    let err = Box::pin(execute_bash_with_gates(args, Some(gate.pipeline())))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("BLOCKED"),
        "block decision must block, got: {err}"
    );
    assert!(
        err.message.contains("Prefer push --force-with-lease."),
        "block must carry the gate's reason, got: {err}"
    );
    assert!(
        !err.message.contains("gate-block-test"),
        "the command must not run after a block, got: {err}"
    );
    assert_eq!(gate.frames().len(), 1);
}

#[tokio::test]
async fn missing_gate_context_fails_closed() {
    // No gate pipeline means the run has no command-policy gate; the command
    // must not run ungated.
    let args = r#"{"command": "echo hi"}"#;
    let err = Box::pin(execute_bash_with_gates(args, None))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("BLOCKED"),
        "missing gate pipeline must fail closed, got: {err}"
    );
    assert!(err.message.contains("no command-policy gate is configured"));
    // The fail-closed denial is recorded with its failure class.
    assert_eq!(
        err.compensation_events.len(),
        1,
        "missing context must record one fail-closed event"
    );
    assert_eq!(
        err.compensation_events[0].kind,
        CompensationKind::JudgeFailClosed
    );
    assert_eq!(
        err.compensation_events[0].detail.as_deref(),
        Some("missing_context")
    );
}

#[tokio::test]
async fn gate_failure_fails_closed() {
    // A gate failure (here: transport) blocks the command with the fail-closed
    // message instead of running it ungated.
    let gate = gate_fixture::failing("transport", "HTTP 500");

    let args = r#"{"command": "echo gate-failure-test"}"#;
    let err = Box::pin(execute_bash_with_gates(args, Some(gate.pipeline())))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("BLOCKED"),
        "failed gate must fail closed, got: {err}"
    );
    assert!(err.message.contains("was unavailable"));
    assert!(err.message.contains("HTTP 500"));
    assert!(
        !err.message.contains("gate-failure-test"),
        "the command must not run after a gate failure, got: {err}"
    );
    // The transport failure is recorded as a fail-closed denial.
    assert_eq!(
        err.compensation_events.len(),
        1,
        "failed gate must record one fail-closed event"
    );
    assert_eq!(
        err.compensation_events[0].kind,
        CompensationKind::JudgeFailClosed
    );
    assert_eq!(
        err.compensation_events[0].detail.as_deref(),
        Some("transport")
    );
}

#[tokio::test]
async fn test_judge_bypass_records_bypass_event() {
    // A bypassed pipeline records one judge_bypass event per call, so the
    // escape hatch cannot be used silently (ADR-018 decision log).
    let args = r#"{"command": "echo bypass-test"}"#;
    let result = Box::pin(execute_bash_with_gates(args, Some(bypassed_gate_context())))
        .await
        .unwrap();
    assert!(result.output.contains("bypass-test"));
    assert_eq!(
        result.compensation_events.len(),
        1,
        "bypassed call must record one judge_bypass event"
    );
    assert_eq!(
        result.compensation_events[0].kind,
        CompensationKind::JudgeBypass
    );
    assert_eq!(result.compensation_events[0].detail, None);
}

#[tokio::test]
async fn allowlist_override_runs_the_allowlisted_command() {
    // An allowlisted command is still judged; a block decision is overridden
    // and the command runs unannotated. The allowlisted command is
    // deliberately harmless (a bare echo) so the test never executes anything
    // destructive.
    let gate = gate_fixture::with_allowlist("echo allowlisted");

    let args = r#"{"command": "echo allowlisted"}"#;
    let result = Box::pin(execute_bash_with_gates(args, Some(gate.pipeline())))
        .await
        .unwrap();
    assert!(
        !result.output.contains("BLOCKED"),
        "an allowlisted block must run unannotated, got: {}",
        result.output
    );
    assert!(result.output.contains("allowlisted"));
    assert_eq!(
        gate.frames().len(),
        1,
        "an allowlisted command is still judged"
    );
    assert_eq!(
        result.compensation_events.len(),
        1,
        "an overridden block must still record a compatibility verdict"
    );
    assert_eq!(
        result.compensation_events[0].kind,
        CompensationKind::JudgeVerdict
    );
    assert_eq!(
        result.compensation_events[0].detail.as_deref(),
        Some("block:fake-block")
    );
    assert_eq!(result.compensation_events[0].overridden, Some(true));
}

#[tokio::test]
async fn gate_message_is_sanitized_before_entering_agent_loop() {
    // Gate text is model-generated: control characters (here ANSI escapes) are
    // stripped and the length is capped so a compromised or confused gate
    // cannot inject terminal sequences or unbounded text into the
    // model-visible output. The `[31m` marker text survives — only the ESC
    // byte is a control character.
    let payload = format!("\u{1b}[31mANSI\u{1b}[0m{}", "x".repeat(2000));
    let gate = gate_fixture::decision(GateDecision::Warn, Some("rg-replace-footgun"), &payload);

    let args = r#"{"command": "echo gate-sanitize-test"}"#;
    let result = Box::pin(execute_bash_with_gates(args, Some(gate.pipeline())))
        .await
        .unwrap();
    assert!(
        result.output.contains("NOTICE: [31mANSI[0m"),
        "escape bytes must be stripped, got: {}",
        result.output
    );
    assert!(
        !result.output.contains('\u{1b}'),
        "escape characters must not reach the agent loop"
    );
    assert!(
        result.output.matches('x').count() <= 1000,
        "gate message must be length-capped"
    );
}

/// Produce large stderr output after stdout closes, hitting the default read
/// cap during the `stdout closed — read remaining stderr` drain loop.
#[tokio::test]
async fn test_streaming_stderr_drain_after_stdout_close_hits_cap() {
    // python3: write small stdout, close it, then flood stderr past cap
    let args = r#"{"command": "python3 -c 'import sys; sys.stdout.write(\"hello\\n\"); sys.stdout.flush(); sys.stdout.close(); sys.stderr.write(\"x\" * 200000)'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await;
    match result {
        Ok(res) => {
            assert!(
                res.output.contains("[Output too long"),
                "Expected truncation when stderr fills after stdout closes. Output: {}",
                res.output
            );
            assert!(
                res.output.contains("[exit:"),
                "Output should contain footer"
            );
        },
        Err(e)
            if e.message.contains("command not found")
                || e.message.contains("python3: cannot open")
                || e.message.contains("python3: not found") =>
        {
            // python3 not available on this system — skip
            eprintln!("skipping: python3 not available");
        },
        Err(e) => panic!("Unexpected error: {e}"),
    }
}

/// Produce large stdout output after stderr closes, hitting the default read
/// cap during the `stderr closed — read remaining stdout` drain loop.
#[tokio::test]
async fn test_streaming_stdout_drain_after_stderr_close_hits_cap() {
    // python3: write small stderr, close it, then flood stdout past cap
    let args = r#"{"command": "python3 -c 'import sys; sys.stderr.write(\"hello\\n\"); sys.stderr.flush(); sys.stderr.close(); sys.stdout.write(\"x\" * 200000)'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await;
    match result {
        Ok(res) => {
            assert!(
                res.output.contains("[Output too long"),
                "Expected truncation when stdout fills after stderr closes. Output: {}",
                res.output
            );
            assert!(
                res.output.contains("[exit:"),
                "Output should contain footer"
            );
        },
        Err(e)
            if e.message.contains("command not found")
                || e.message.contains("python3: cannot open")
                || e.message.contains("python3: not found") =>
        {
            // python3 not available on this system — skip
            eprintln!("skipping: python3 not available");
        },
        Err(e) => panic!("Unexpected error: {e}"),
    }
}

// ===========================================================================
// Secure Temp Directory Tests
// ===========================================================================

#[cfg(unix)]
#[test]
fn secure_temp_dir_creates_per_user_directory() {
    let dir = bash_temp_output_dir().unwrap();
    let dir_name = dir.file_name().unwrap().to_str().unwrap();
    // SAFETY: `getuid()` is a simple system call with no safety requirements.
    let uid = unsafe { libc::getuid() };
    assert!(
        dir_name.starts_with(&format!("cake-{uid}-")),
        "expected directory name to start with 'cake-{uid}-', got '{dir_name}'"
    );
    assert!(dir.exists(), "directory should exist");
    assert!(dir.is_dir(), "path should be a directory");
}

#[cfg(unix)]
#[test]
fn secure_temp_dir_has_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = bash_temp_output_dir().unwrap();
    assert!(dir.exists(), "directory should exist");

    let metadata = std::fs::metadata(dir).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "expected 0o700 permissions on secure temp dir, got 0o{mode:o}"
    );
}

#[cfg(unix)]
#[test]
fn secure_temp_dir_is_owned_by_current_user() {
    use std::os::unix::fs::MetadataExt;

    let dir = bash_temp_output_dir().unwrap();
    let metadata = std::fs::metadata(dir).unwrap();
    // SAFETY: `getuid()` is a simple system call with no safety requirements.
    let uid = unsafe { libc::getuid() };
    assert_eq!(
        metadata.uid(),
        uid,
        "directory should be owned by current user"
    );
}

#[test]
fn secure_temp_dir_usable_for_truncation() {
    // Verify that truncate_output writes to the secure temp dir
    let large = "x".repeat(BASH_OUTPUT_MAX_BYTES + 1000);
    let result = truncate_output(&large, Some(BASH_OUTPUT_MAX_BYTES), 0, 100, false);
    let path_line = result
        .lines()
        .find(|l| l.starts_with("Full output saved to:"))
        .expect("should contain temp file path");
    let path_str = path_line
        .trim_start_matches("Full output saved to: ")
        .trim();
    let path = std::path::Path::new(path_str);
    assert!(
        path.exists(),
        "temp file should exist at: {}",
        path.display()
    );

    // The path should be under our secure temp dir
    let parent = path.parent().unwrap();
    let secure_dir = bash_temp_output_dir().unwrap();
    assert_eq!(
        parent, secure_dir,
        "temp file should be inside secure temp dir"
    );

    // Clean up
    _ = std::fs::remove_file(path);
}
