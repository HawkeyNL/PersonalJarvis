use super::*;
use clap::Parser;

#[test]
fn release_tags_are_strict() {
    assert!(valid_release_tag("v0.0.11"));
    assert!(!valid_release_tag("v1.2"));
    assert!(!valid_release_tag("v1.2.3;id"));
}
#[test]
fn clap_rejects_unknown_root_command() {
    assert!(Cli::try_parse_from(["jarvis", "shell"]).is_err());
}
#[test]
fn bare_cli_and_bare_json_have_no_implicit_command() {
    let bare = Cli::try_parse_from(["jarvis"]).unwrap();
    assert!(bare.command.is_none());
    assert!(!bare.json);

    let json = Cli::try_parse_from(["jarvis", "--json"]).unwrap();
    assert!(json.command.is_none());
    assert!(json.json);
}
#[test]
fn clap_bounds_log_lines() {
    assert!(Cli::try_parse_from(["jarvis", "logs", "core", "--lines", "0"]).is_err());
}
#[test]
fn update_modes_cannot_be_combined() {
    assert!(Cli::try_parse_from(["jarvis", "update", "--latest", "--check"]).is_err());
}

#[test]
fn bare_update_is_a_center_but_explicit_modes_are_not() {
    let bare = Cli::try_parse_from(["jarvis", "update"]).unwrap();
    let Some(Commands::Update(bare)) = bare.command else {
        panic!("expected update command");
    };
    assert_eq!(UpdateInvocation::from_args(&bare), UpdateInvocation::Center);

    let check = Cli::try_parse_from(["jarvis", "update", "--check"]).unwrap();
    let Some(Commands::Update(check)) = check.command else {
        panic!("expected update command");
    };
    assert_eq!(UpdateInvocation::from_args(&check), UpdateInvocation::Check);

    let latest = Cli::try_parse_from(["jarvis", "update", "--latest"]).unwrap();
    let Some(Commands::Update(latest)) = latest.command else {
        panic!("expected update command");
    };
    assert_eq!(
        UpdateInvocation::from_args(&latest),
        UpdateInvocation::Latest
    );
}
#[test]
fn log_target_is_allowlisted() {
    assert!(Cli::try_parse_from(["jarvis", "logs", "arbitrary.service"]).is_err());
}
#[test]
fn no_color_disables_interactive_rendering() {
    assert!(!terminal_supports_rich_output_for(
        true,
        false,
        Some("xterm-256color")
    ));
    assert!(!terminal_supports_rich_output_for(true, true, Some("dumb")));
    assert!(!terminal_supports_rich_output_for(
        false,
        true,
        Some("xterm-256color")
    ));
    assert!(terminal_supports_rich_output_for(
        true,
        true,
        Some("xterm-256color")
    ));
    assert!(!Presentation::new(true, true).interactive);
}

#[test]
fn tui_exit_reason_requires_documented_pressed_keys() {
    use crossterm::event::{KeyEvent, KeyEventState};

    let pressed = |code, modifiers| {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    };
    assert_eq!(
        close_exit_reason(&pressed(KeyCode::Char('q'), KeyModifiers::NONE)),
        Some(TuiExitReason::Quit)
    );
    assert_eq!(
        close_exit_reason(&pressed(KeyCode::Esc, KeyModifiers::NONE)),
        Some(TuiExitReason::Escape)
    );
    assert_eq!(
        close_exit_reason(&pressed(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(TuiExitReason::CtrlC)
    );
    assert_eq!(
        close_exit_reason(&pressed(KeyCode::Char('c'), KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        close_exit_reason(&Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        })),
        None
    );
}

#[test]
fn tui_trace_never_records_pasted_contents() {
    let mut trace = TuiTrace::new(true);
    trace.record_event(&Event::Paste("fixture-secret-must-not-appear".to_owned()));
    let rendered = trace.entries.into_iter().collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("contents omitted"));
    assert!(!rendered.contains("fixture-secret-must-not-appear"));
}

#[test]
fn typed_model_input_rejects_newline_injection() {
    assert!(Cli::try_parse_from(["jarvis", "models", "enable", "openai-api", "x\ny"]).is_err());
}

#[test]
fn huggingface_route_cli_is_typed_and_rejects_shell_or_url_input() {
    assert!(Cli::try_parse_from([
        "jarvis",
        "models",
        "set-route",
        "huggingface",
        "openai/gpt-oss-20b",
        "groq",
    ])
    .is_ok());
    for route in ["https://evil", "groq;sh", "a/b", "line\nbreak"] {
        assert!(Cli::try_parse_from([
            "jarvis",
            "models",
            "set-route",
            "huggingface",
            "openai/gpt-oss-20b",
            route,
        ])
        .is_err());
    }
}

fn admin_helper_layout(
    admin_helpers: bool,
) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, u32, u32) {
    let directory = tempfile::tempdir().unwrap();
    let owner = fs::metadata(directory.path()).unwrap();
    let releases = directory.path().join("releases");
    let tag = if admin_helpers { "v0.0.20" } else { "v0.0.19" };
    let release = releases.join(tag);
    let current = directory.path().join("current");
    let legacy = directory.path().join("legacy-sbin");
    fs::create_dir_all(&release).unwrap();
    fs::create_dir_all(&legacy).unwrap();
    fs::set_permissions(&releases, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&release, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o755)).unwrap();
    let manifest = if admin_helpers {
        r#"{"tag":"v0.0.20","tooling":{"admin_helpers":1}}"#
    } else {
        r#"{"tag":"v0.0.19","tooling":{"private_agents":1}}"#
    };
    fs::write(release.join("release.json"), manifest).unwrap();
    fs::set_permissions(
        release.join("release.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    for name in ["jarvis-models", "jarvis-credentials"] {
        fs::write(release.join(name), format!("versioned {name}\n")).unwrap();
        fs::set_permissions(release.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(legacy.join(name), format!("legacy {name}\n")).unwrap();
        fs::set_permissions(legacy.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::os::unix::fs::symlink(&release, &current).unwrap();
    (
        directory,
        current,
        releases,
        legacy,
        owner.uid(),
        owner.gid(),
    )
}

#[test]
fn versioned_admin_helpers_come_from_the_active_release() {
    let (_directory, current, releases, legacy, uid, gid) = admin_helper_layout(true);
    let models =
        resolve_admin_helper(&current, &releases, &legacy, AdminHelper::Models, uid, gid).unwrap();
    let credentials = resolve_admin_helper(
        &current,
        &releases,
        &legacy,
        AdminHelper::Credentials,
        uid,
        gid,
    )
    .unwrap();
    assert_eq!(models, releases.join("v0.0.20/jarvis-models"));
    assert_eq!(credentials, releases.join("v0.0.20/jarvis-credentials"));
    assert!(fs::read_to_string(models).unwrap().starts_with("versioned"));
    assert!(fs::read_to_string(credentials)
        .unwrap()
        .starts_with("versioned"));
}

#[test]
fn legacy_release_without_capability_uses_fixed_compatibility_paths() {
    let (_directory, current, releases, legacy, uid, gid) = admin_helper_layout(false);
    let helper =
        resolve_admin_helper(&current, &releases, &legacy, AdminHelper::Models, uid, gid).unwrap();
    assert_eq!(helper, legacy.join("jarvis-models"));
}

#[test]
fn admin_helper_resolution_rejects_escape_symlink_and_unsafe_mode() {
    let (directory, current, releases, legacy, uid, gid) = admin_helper_layout(true);
    let active = releases.join("v0.0.20");
    let helper = active.join("jarvis-models");

    fs::remove_file(&helper).unwrap();
    std::os::unix::fs::symlink(legacy.join("jarvis-models"), &helper).unwrap();
    assert!(
        resolve_admin_helper(&current, &releases, &legacy, AdminHelper::Models, uid, gid).is_err()
    );

    fs::remove_file(&helper).unwrap();
    fs::write(&helper, "unsafe\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(
        resolve_admin_helper(&current, &releases, &legacy, AdminHelper::Models, uid, gid).is_err()
    );

    let outside = directory.path().join("v0.0.20-outside");
    fs::create_dir(&outside).unwrap();
    fs::remove_file(&current).unwrap();
    std::os::unix::fs::symlink(outside, &current).unwrap();
    assert!(resolve_admin_helper(
        &current,
        &releases,
        &legacy,
        AdminHelper::Credentials,
        uid,
        gid
    )
    .is_err());
}

#[test]
fn arbitrary_admin_helper_name_is_rejected() {
    assert_eq!(
        AdminHelper::from_name("jarvis-models").unwrap(),
        AdminHelper::Models
    );
    assert!(AdminHelper::from_name("../../bin/sh").is_err());
    assert!(AdminHelper::from_name("arbitrary-helper").is_err());
}

#[test]
fn explicit_credential_and_model_helpers_use_normal_terminal_output() {
    assert_eq!(
        explicit_helper_subprocess_mode(false),
        SubprocessMode::InheritedInteractive
    );
    assert_eq!(
        explicit_helper_subprocess_mode(true),
        SubprocessMode::Streamed
    );
}

#[test]
fn mutation_lock_is_exclusive() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("admin.lock");
    let _first = mutation_lock(&path).unwrap();
    assert!(mutation_lock(&path).is_err());
}

#[test]
fn child_environment_is_minimal() {
    let command = trusted_command("true");
    let environment: BTreeMap<_, _> = command
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
        .collect();
    assert_eq!(environment.get(OsStr::new("HOME")), Some(&"/root".into()));
    assert!(!environment.contains_key(OsStr::new("JARVIS_LLM_OPENAI_API_KEY")));
}

#[test]
fn repository_allowlist_rejects_shell_syntax() {
    assert!(valid_repository("HawkeyNL/PersonalJarvis"));
    assert!(!valid_repository("HawkeyNL/PersonalJarvis;id"));
    assert!(!valid_repository("../../etc/passwd"));
}

#[test]
fn trusted_updater_config_is_strict_and_never_shell_parsed() {
    let config = parse_updater_config(
        "JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis\nJARVIS_UPDATE_CHANNEL=stable\n",
    )
    .unwrap();
    assert_eq!(config.repository, "HawkeyNL/PersonalJarvis");
    assert!(parse_updater_config("JARVIS_UPDATE_REPOSITORY=bad;id\n").is_err());
    assert!(parse_updater_config("JARVIS_UPDATE_REPOSITORY=a/b\nUNKNOWN=x\n").is_err());
    assert!(
        parse_updater_config("JARVIS_UPDATE_REPOSITORY=a/b\nJARVIS_UPDATE_REPOSITORY=c/d\n")
            .is_err()
    );
    assert!(parse_updater_config(
        "JARVIS_UPDATE_REPOSITORY=a/b\nJARVIS_GITHUB_CURL_NETRC=relative\n"
    )
    .is_err());
}

#[test]
fn confirmation_is_explicit_and_non_secret() {
    assert!(confirmation_answer("yes\n"));
    assert!(confirmation_answer("Y"));
    assert!(!confirmation_answer(""));
    assert!(!confirmation_answer("yes; id"));
}

#[test]
fn dashboard_renders_compactly_on_narrow_terminals() {
    use ratatui::{backend::TestBackend, Terminal};

    let report = StatusReport {
        release: Some("v0.0.13".to_owned()),
        services: BTreeMap::from([("Core", "active".to_owned())]),
        updater_enabled: "enabled".to_owned(),
        agent_bundle: None,
    };
    let backend = TestBackend::new(32, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_status_dashboard(frame, &report))
        .unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Release:"));
    assert!(rendered.contains("Core:"));
}

#[test]
fn dashboard_includes_codex_broker_state() {
    use ratatui::{backend::TestBackend, Terminal};

    let report = StatusReport {
        release: Some("v0.0.14".to_owned()),
        services: BTreeMap::from([
            ("Core", "active".to_owned()),
            ("Codex broker", "active".to_owned()),
        ]),
        updater_enabled: "enabled".to_owned(),
        agent_bundle: None,
    };
    let backend = TestBackend::new(80, 18);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_status_dashboard(frame, &report))
        .unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Codex broker"));
}

#[test]
fn updater_plain_status_has_strict_json_conversion() {
    let values = parse_key_value_output(
        "Current:  v0.0.13\nPrevious: v0.0.12\nLatest: v0.0.14\nCore current: 0.1.0\nCore latest: 0.1.1\nCLI current: 0.1.0\nCLI latest: 0.1.0\nCore app current: 0.1.0\nCore app latest: 0.2.0\nUpdater: enabled\n",
    )
    .unwrap();
    assert_eq!(values.get("current").map(String::as_str), Some("v0.0.13"));
    assert_eq!(values.get("previous").map(String::as_str), Some("v0.0.12"));
    assert_eq!(
        values.get("core_app_latest").map(String::as_str),
        Some("0.2.0")
    );
    assert!(parse_key_value_output("not structured\n").is_err());
    assert!(parse_key_value_output("Current: one\nCurrent: two\n").is_err());
}

#[test]
fn update_summary_merges_status_and_check_without_ui_side_effects() {
    let mut summary = UpdateSummary::default();
    summary
        .merge_helper_output(
            "jarvis updater: resolved stable release v0.0.16\nCurrent: v0.0.15\nPrevious: v0.0.14\nLatest: v0.0.16\nUpdater: enabled\n",
        )
        .unwrap();
    assert_eq!(summary.current.as_deref(), Some("v0.0.15"));
    assert_eq!(summary.previous.as_deref(), Some("v0.0.14"));
    assert_eq!(summary.update_available, Some(true));
    summary
        .merge_helper_output(
            "Current: v0.0.15\nLatest: v0.0.16\nCore current: 0.1.0\nCore latest: 0.1.0\nCLI current: 0.1.0\nCLI latest: 0.1.0\nCore app current: 0.1.0\nCore app latest: 0.2.0\nUpdate: available\n",
        )
        .unwrap();
    assert_eq!(summary.core_app_current.as_deref(), Some("0.1.0"));
    assert_eq!(summary.core_app_latest.as_deref(), Some("0.2.0"));
    summary
        .merge_helper_output("Current: v0.0.16\nLatest: v0.0.16\nUpdate: not available\n")
        .unwrap();
    assert_eq!(summary.update_available, Some(false));
    assert_eq!(summary.updater.as_deref(), Some("enabled"));
}

#[test]
fn release_version_comparison_is_strict() {
    assert!(valid_component_version("0.1.0"));
    assert!(!valid_component_version("v0.1.0"));
    assert!(!valid_component_version("0.1.0;id"));
    assert!(release_is_newer("v0.0.16", "v0.0.15"));
    assert!(release_is_newer("v1.0.0", "v0.99.99"));
    assert!(!release_is_newer("v0.0.15", "v0.0.15"));
    assert!(!release_is_newer("not-a-tag", "v0.0.15"));
}

#[test]
fn update_center_renders_narrow_fixture_and_unavailable_history() {
    use ratatui::{backend::TestBackend, Terminal};

    let mut center = UpdateCenter::base(Some(FixtureUpdateMode {
        fail_mutations: false,
    }));
    center.summary.current = Some("v0.0.15".to_owned());
    center.summary.latest = Some("v0.0.16".to_owned());
    center.candidates = vec![RollbackCandidate {
        version: "v0.0.10".to_owned(),
        current: false,
        verified: false,
        rollback_capable: false,
        reason: "verification marker is missing or invalid".to_owned(),
    }];
    center.screen = UpdateScreen::RollbackSelection;
    let backend = TestBackend::new(44, 14);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new(update_screen_lines(&center)), frame.area());
        })
        .unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Rollback candidates"));
    assert!(rendered.contains("v0.0.10"));
    assert!(!rendered.contains("credential"));
    assert!(!rendered.contains("api_key"));
}

#[test]
fn update_progress_is_bounded_and_strips_terminal_controls() {
    let mut messages = VecDeque::new();
    for index in 0..40 {
        push_bounded(&mut messages, format!("line {index}"), 18);
    }
    assert_eq!(messages.len(), 18);
    assert_eq!(
        sanitize_terminal_line("safe\u{1b}[31msecret"),
        "safe[31msecret"
    );
}

#[test]
fn successful_live_mutation_requires_the_old_tui_to_exit() {
    use std::os::unix::process::ExitStatusExt;

    let mut center = UpdateCenter::base(None);
    center
        .complete(ChildOutcome {
            operation: UpdateOperation::Version("v9.8.7".to_owned()),
            status: ExitStatus::from_raw(0),
            stdout: String::new(),
            stderr: String::new(),
        })
        .unwrap();
    assert_eq!(
        center.take_client_replacement().as_deref(),
        Some("Updated successfully to v9.8.7")
    );
    assert!(center.take_client_replacement().is_none());

    center
        .complete(ChildOutcome {
            operation: UpdateOperation::Check,
            status: ExitStatus::from_raw(0),
            stdout: "Current: v9.8.7\nLatest: v9.8.7\nUpdate: not available\n".to_owned(),
            stderr: String::new(),
        })
        .unwrap();
    assert!(center.take_client_replacement().is_none());

    center
        .complete(ChildOutcome {
            operation: UpdateOperation::Latest,
            status: ExitStatus::from_raw(256),
            stdout: String::new(),
            stderr: "activation failed; restored".to_owned(),
        })
        .unwrap();
    assert!(center.take_client_replacement().is_none());
}

#[test]
fn rollback_selection_requires_eligible_target_and_explicit_confirmation() {
    let mut center = UpdateCenter::base(Some(FixtureUpdateMode {
        fail_mutations: false,
    }));
    center.screen = UpdateScreen::RollbackSelection;
    center.candidates = vec![
        RollbackCandidate {
            version: "v0.0.10".to_owned(),
            current: false,
            verified: false,
            rollback_capable: false,
            reason: "invalid legacy release".to_owned(),
        },
        RollbackCandidate {
            version: "v0.0.14".to_owned(),
            current: false,
            verified: true,
            rollback_capable: true,
            reason: "eligible".to_owned(),
        },
    ];

    center.activate_selection().unwrap();
    assert_eq!(center.screen, UpdateScreen::RollbackSelection);
    assert!(center.confirmation.is_none());

    center.selected = 1;
    center.activate_selection().unwrap();
    assert_eq!(center.screen, UpdateScreen::RollbackConfirm);
    assert_eq!(center.selected, 0, "confirmation must default to Cancel");
    center.activate_selection().unwrap();
    assert_eq!(center.screen, UpdateScreen::RollbackSelection);

    center.selected = 1;
    center.activate_selection().unwrap();
    center.selected = 1;
    center.activate_selection().unwrap();
    assert_eq!(center.screen, UpdateScreen::Running);
    assert!(matches!(
        center.operation,
        Some(UpdateOperation::Rollback(ref version)) if version == "v0.0.14"
    ));
}

#[test]
fn model_policy_json_round_trips_without_credentials() {
    let policy: ModelPolicy = serde_json::from_str(
        r#"{"version":1,"models":[{"provider":"openai-api","model":"gpt-test","enabled":false,"source":"fixture"}]}"#,
    )
    .unwrap();
    assert_eq!(policy.models.len(), 1);
    let output = serde_json::to_string(&policy).unwrap();
    assert!(!output.contains("credential"));
    assert!(!output.contains("api_key"));
}

#[test]
fn agent_tree_manifest_projection_ignores_private_or_unknown_fields() {
    let manifest = br#"{
        "version":1,
        "bundle_id":"bundle-fixture",
        "agents":[{
            "id":"research",
            "path":"agents/research.json",
            "sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "name":"Research",
            "group":"Development",
            "model_policy":"research",
            "profile_lines":142,
            "source_updated_at":"2026-08-29T14:32:00+02:00",
            "instructions":"fixture-must-never-render",
            "api_key":"fixture-secret"
        }]
    }"#;
    let tree = parse_safe_agent_manifest(manifest, "bundle-fixture").unwrap();
    assert_eq!(tree.agents[0].group.as_deref(), Some("Development"));
    assert_eq!(tree.agents[0].name, "Research");
    assert_eq!(tree.agents[0].profile_lines, Some(142));
    assert_eq!(
        tree.agents[0].source_updated_at.as_deref(),
        Some("2026-08-29T14:32:00+02:00")
    );
    let retained = format!("{tree:?}");
    assert!(!retained.contains("fixture-must-never-render"));
    assert!(!retained.contains("fixture-secret"));
    let projected = serde_json::to_string(&tree.agents).unwrap();
    assert!(!projected.contains("instructions"));
    assert!(!projected.contains("api_key"));
    assert!(!projected.contains("path"));
    assert!(!projected.contains("sha256"));

    let legacy = br#"{
        "version":1,
        "bundle_id":"bundle-fixture",
        "agents":[{
            "id":"research",
            "path":"agents/research.json",
            "sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }]
    }"#;
    let legacy_tree = parse_safe_agent_manifest(legacy, "bundle-fixture").unwrap();
    assert_eq!(legacy_tree.agents[0].name, "research");
    assert_eq!(legacy_tree.agents[0].group, None);
    assert_eq!(legacy_tree.agents[0].profile_lines, None);
    assert_eq!(legacy_tree.agents[0].source_updated_at, None);
}

#[test]
fn agent_tree_manifest_rejects_unsafe_safe_metadata() {
    let invalid = br#"{
        "version":1,
        "bundle_id":"bundle-fixture",
        "agents":[{
            "id":"research",
            "path":"agents/research.json",
            "sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "profile_lines":0,
            "source_updated_at":"2026-08-29T14:32:00Z\nsecret"
        }]
    }"#;
    assert!(parse_safe_agent_manifest(invalid, "bundle-fixture").is_err());
}

#[test]
fn tooling_pair_replaces_both_completed_files() {
    let directory = tempfile::tempdir().unwrap();
    let admin_source = directory.path().join("admin-source");
    let updater_source = directory.path().join("updater-source");
    let admin_destination = directory.path().join("jarvis");
    let updater_destination = directory.path().join("updater");
    fs::write(&admin_source, "verified admin").unwrap();
    fs::write(&updater_source, "verified updater").unwrap();
    fs::write(&admin_destination, "old admin").unwrap();
    fs::write(&updater_destination, "old updater").unwrap();
    install_tooling_pair(
        &admin_source,
        &admin_destination,
        &updater_source,
        &updater_destination,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&admin_destination).unwrap(),
        "verified admin"
    );
    assert_eq!(
        fs::read_to_string(&updater_destination).unwrap(),
        "verified updater"
    );
    assert_eq!(
        fs::metadata(admin_destination)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn tooling_pair_preflight_failure_keeps_both_installed_tools() {
    let directory = tempfile::tempdir().unwrap();
    let admin_source = directory.path().join("admin-source");
    let updater_source = directory.path().join("updater-source");
    let admin_destination = directory.path().join("jarvis");
    let updater_destination = directory.path().join("updater");
    fs::write(&admin_source, "verified admin").unwrap();
    fs::write(&updater_source, "verified updater").unwrap();
    fs::create_dir(&admin_destination).unwrap();
    fs::write(&updater_destination, "old updater").unwrap();
    assert!(install_tooling_pair(
        &admin_source,
        &admin_destination,
        &updater_source,
        &updater_destination,
    )
    .is_err());
    assert!(admin_destination.is_dir());
    assert_eq!(
        fs::read_to_string(updater_destination).unwrap(),
        "old updater"
    );
}

#[test]
fn failed_legacy_migration_rolls_back_only_new_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let created = directory.path().join("created.env");
    let existing = directory.path().join("existing.env");
    fs::write(&created, "new").unwrap();
    fs::write(&existing, "existing").unwrap();
    rollback_new_updater_config(true, &created).unwrap();
    rollback_new_updater_config(false, &existing).unwrap();
    assert!(!created.exists());
    assert_eq!(fs::read_to_string(existing).unwrap(), "existing");
}
