use ccnm_core::Config;
use ccnm_core::instance::{AgentProfiles, InstanceRef, WorkspaceBinding};
use ccnm_core::provider::AgentProvider;
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p2-{}", ccnm_core::session::new_id()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const RUNTIME: &str = include_str!("../../../tests/fixtures/agent-instance/runtime.toml");
const AGENT: &str = include_str!("../../../tests/fixtures/agent-instance/agent.toml");
const PROFILES: &str = include_str!("../../../tests/fixtures/agent-instance/profiles.toml");
const LEGACY: &str = include_str!("../../../tests/fixtures/agent-instance/legacy-runtime.toml");

#[test]
fn runtime_accepts_a_reference_without_a_copy_of_the_agent_registry() {
    assert!(Config::parse(RUNTIME).is_ok());
}

#[test]
fn one_node_can_register_two_provider_instances_without_workspace_roots() {
    assert!(Config::parse(AGENT).is_ok());
}

fn reference(instance: &str) -> InstanceRef {
    InstanceRef {
        node: "worker".into(),
        instance: instance.into(),
    }
}

#[test]
fn both_instances_resolve_without_creating_or_moving_profile_state() {
    let config = Config::parse(AGENT).unwrap();
    let profiles = AgentProfiles::default();
    let f = Fixture::new();
    for (name, provider, suffix) in [
        ("claude-main", AgentProvider::Claude, ".claude"),
        (
            "codex-main",
            AgentProvider::Codex,
            ".config/ccnm/agents/codex",
        ),
    ] {
        let resolved = config
            .resolve_instance(&reference(name), &profiles, &f.0, None)
            .unwrap();
        assert_eq!(resolved.identity().provider, provider);
        assert_eq!(resolved.identity().node, "worker");
        assert_eq!(resolved.identity().profile_ref, "default");
        assert_eq!(resolved.profile().directory(), f.0.join(suffix));
        assert!(!resolved.profile().directory().exists());
    }
    let xdg = f.0.join("xdg");
    let resolved = config
        .resolve_instance(&reference("codex-main"), &profiles, &f.0, Some(&xdg))
        .unwrap();
    assert_eq!(
        resolved.profile().directory(),
        xdg.join("ccnm/agents/codex")
    );
    assert!(!xdg.exists());
}

#[test]
fn configuration_roundtrips_without_private_profile_definitions() {
    for text in [RUNTIME, AGENT, LEGACY] {
        let config = Config::parse(text).unwrap();
        let serialized = toml::to_string(&config).unwrap();
        assert_eq!(Config::parse(&serialized).unwrap(), config);
        assert!(!serialized.contains("/synthetic/agent/private"));
    }
    assert!(Config::parse(LEGACY).unwrap().workspace("demo").is_ok());
    let registered = Config::parse(&format!(
        "{LEGACY}\n[agents.local-codex]\nprovider='codex'\nprofile_ref='default'\n"
    ))
    .unwrap();
    assert_eq!(
        registered.workspace("demo").unwrap().workspace.agent_node,
        "worker",
        "registration alone must not change legacy selection"
    );
    assert_eq!(
        Config::parse(RUNTIME)
            .unwrap()
            .workspace("demo")
            .unwrap()
            .agent_node(),
        "worker"
    );
}

#[test]
fn new_and_legacy_selectors_and_permissions_do_not_mix() {
    for text in [
        RUNTIME.replace("root =", "agent_node = \"worker\"\nroot ="),
        RUNTIME.replace(
            "root =",
            "claude_permission_mode = \"bypassPermissions\"\nroot =",
        ),
        RUNTIME.replace(
            "ssh = \"agent-alias\"",
            "ssh = \"agent-alias\"\nclaude_config_dir = \"/legacy/private\"",
        ),
    ] {
        assert!(Config::parse(&text).is_err());
    }
    // Existing legacy custom profiles and policy remain parseable.
    assert!(
        Config::parse(&LEGACY.replace(
            "agent_node = \"worker\" #",
            "claude_permission_mode = \"plan\"\nagent_node = \"worker\" #"
        ))
        .is_ok()
    );
}

#[test]
fn unknown_duplicate_conflicting_and_path_shaped_references_are_rejected() {
    for text in [
        AGENT.replace("provider = \"codex\"", "provider = \"unknown\""),
        AGENT.replace(
            "provider = \"codex\"",
            "provider = \"codex\"\nprovider = \"claude\"",
        ),
        format!("{AGENT}\n[agents.codex-main]\nprovider=\"codex\"\nprofile_ref=\"default\""),
        AGENT.replace(
            "profile_ref = \"default\"",
            "profile_ref = \"/private/path\"",
        ),
        AGENT.replace(
            "profile_ref = \"default\"",
            "profile_ref = \"default\"\ndirectory=\"/private/path\"",
        ),
        AGENT.replace(
            "[agents.codex-main]",
            "[agents.codex-main]\nnode=\"another-node\"",
        ),
        RUNTIME.replace("node = \"worker\"", "node = \"unknown\""),
        RUNTIME.replace(
            "instance = \"claude-main\"",
            "instance = \"../claude-main\"",
        ),
        RUNTIME.replace(
            "instance = \"claude-main\"",
            &format!("instance = \"{}\"", "a".repeat(65)),
        ),
    ] {
        assert!(Config::parse(&text).is_err(), "{text}");
    }
}

#[test]
fn unknown_remote_instances_fail_on_agent_and_wrong_node_never_loads_profiles() {
    let runtime = Config::parse(&RUNTIME.replace("claude-main", "missing")).unwrap();
    let target = runtime.instance_reference("demo").unwrap();
    let agent = Config::parse(AGENT).unwrap();
    assert!(
        agent
            .resolve_instance(
                &target,
                &AgentProfiles::default(),
                Path::new("/agent"),
                None
            )
            .is_err()
    );
    let wrong = InstanceRef {
        node: "elsewhere".into(),
        instance: "codex-main".into(),
    };
    let error = agent.resolve_instance_local(&wrong).err().unwrap();
    assert!(error.message().contains("another Agent Node"));
}

#[test]
fn profile_provider_mismatch_and_unknown_reference_never_fall_back() {
    let profiles = AgentProfiles::parse(PROFILES).unwrap();
    let agent = Config::parse(&AGENT.replace(
        "profile_ref = \"default\"",
        "profile_ref = \"claude-extra\"",
    ))
    .unwrap();
    let home = Path::new("/synthetic/home");
    let claude = agent
        .resolve_instance(&reference("claude-main"), &profiles, home, None)
        .unwrap();
    assert_eq!(
        claude.profile().directory(),
        Path::new("/synthetic/agent/private/claude-extra")
    );
    assert!(
        agent
            .resolve_instance(&reference("codex-main"), &profiles, home, None)
            .is_err()
    );
    assert!(
        agent
            .resolve_instance(
                &reference("claude-main"),
                &AgentProfiles::default(),
                home,
                None
            )
            .is_err()
    );
    for text in [
        PROFILES.replace("claude-extra", "default"),
        PROFILES.replace("/synthetic/agent/private/claude-extra", "../private"),
        format!("{PROFILES}\n[profiles.claude-extra]\nprovider='claude'\ndirectory='/other'"),
        "[profiles.extra]\nprovider='codex'\ndirectory='/private'\nauth='SYNTHETIC_SECRET'".into(),
    ] {
        let error = AgentProfiles::parse(&text).err().unwrap();
        assert!(!error.message().contains("SYNTHETIC_SECRET"));
        assert!(!error.message().contains("/synthetic/agent/private"));
    }
}

#[test]
fn named_profile_preflight_rejects_symlinks_without_copying_auth() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new();
    let real = f.0.join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = f.0.join("link");
    symlink(&real, &link).unwrap();
    let text = format!(
        "[profiles.extra]\nprovider='codex'\ndirectory='{}'",
        link.display()
    );
    let profiles = AgentProfiles::parse(&text).unwrap();
    let agent =
        Config::parse(&AGENT.replace("profile_ref = \"default\"", "profile_ref = \"extra\""))
            .unwrap();
    let resolved = agent
        .resolve_instance(&reference("codex-main"), &profiles, &f.0, None)
        .unwrap();
    assert!(resolved.profile().validate_private_directory().is_err());
    assert!(!real.join("auth.json").exists());
}

fn bound() -> (Config, Config, WorkspaceBinding) {
    let runtime = Config::parse(&RUNTIME.replace("claude-main", "codex-main")).unwrap();
    let agent = Config::parse(AGENT).unwrap();
    let resolved = agent
        .resolve_instance(
            &runtime.instance_reference("demo").unwrap(),
            &AgentProfiles::default(),
            Path::new("/agent"),
            None,
        )
        .unwrap();
    let binding = runtime.bind_workspace("demo", resolved.identity()).unwrap();
    (runtime, agent, binding)
}

#[test]
fn two_authorities_bind_identity_and_runtime_root_without_private_fields() {
    let (runtime, agent, binding) = bound();
    runtime.verify_runtime_binding(&binding).unwrap();
    let resolved = agent
        .resolve_bound_instance(
            &binding,
            &AgentProfiles::default(),
            Path::new("/agent"),
            None,
        )
        .unwrap();
    assert_eq!(resolved.identity(), &binding.agent);
    let value = serde_json::to_value(&binding).unwrap();
    assert_eq!(
        value,
        serde_json::from_str::<serde_json::Value>(include_str!(
            "../../../tests/fixtures/agent-instance/binding.json"
        ))
        .unwrap()
    );
    let text = value.to_string();
    for private in [
        "directory",
        "CODEX_HOME",
        "claude_config_dir",
        "/agent/.config",
    ] {
        assert!(!text.contains(private));
    }
    let roundtrip: WorkspaceBinding = serde_json::from_value(value).unwrap();
    assert_eq!(roundtrip, binding);
}

#[test]
fn substitutions_and_stale_registry_identity_fail_on_the_authoritative_side() {
    let (runtime, agent, binding) = bound();
    let home = Path::new("/agent");
    let profiles = AgentProfiles::default();
    let mut changed = binding.clone();
    changed.root = "/runtime/other".into();
    assert!(runtime.verify_runtime_binding(&changed).is_err());
    let mut changed = binding.clone();
    changed.agent.provider = AgentProvider::Claude;
    assert!(
        agent
            .resolve_bound_instance(&changed, &profiles, home, None)
            .is_err()
    );
    let mut changed = binding.clone();
    changed.agent.instance = "claude-main".into();
    runtime.verify_runtime_binding(&changed).unwrap();
    assert!(
        agent
            .resolve_bound_instance(&changed, &profiles, home, None)
            .is_err(),
        "same-node override is Runtime-authorized but still Agent-resolved"
    );
    let mut changed = binding.clone();
    changed.runtime_node = "other".into();
    assert!(
        agent
            .resolve_bound_instance(&changed, &profiles, home, None)
            .is_err()
    );
    let mut changed = binding.clone();
    changed.agent.profile_ref = "other".into();
    assert!(
        agent
            .resolve_bound_instance(&changed, &profiles, home, None)
            .is_err()
    );
    let mut forged = serde_json::to_value(binding).unwrap();
    forged["agent"]["directory"] = "/private/injected".into();
    assert!(serde_json::from_value::<WorkspaceBinding>(forged).is_err());
}

/// `model` is Codex's, and only Codex's.
///
/// ccnm passes nothing of the sort to Claude -- its model is part of its own
/// configuration -- so accepting the key there would set something no launch
/// ever reads. It is also going onto a command line, so it is checked for
/// that before anything tries to use it.
#[test]
fn only_codex_instances_may_name_a_model_and_the_value_is_checked() {
    let base = "this='agent'\nruntime_node='runtime'\n[nodes.agent]\n[nodes.runtime]\nssh='r'\n";
    let codex = Config::parse(&format!(
        "{base}[agents.codex-main]\nprovider='codex'\nprofile_ref='default'\nmodel='gpt-5.3-codex-spark'\n"
    ))
    .expect("a Codex instance may name a model");
    assert_eq!(
        codex.agents["codex-main"].model.as_deref(),
        Some("gpt-5.3-codex-spark")
    );

    let claude = Config::parse(&format!(
        "{base}[agents.claude-main]\nprovider='claude'\nprofile_ref='default'\nmodel='whatever'\n"
    ));
    assert!(claude.is_err(), "Claude instances must refuse it");

    for bad in ["", "gpt 5", "a;b", "$(x)"] {
        let refused = Config::parse(&format!(
            "{base}[agents.codex-main]\nprovider='codex'\nprofile_ref='default'\nmodel='{bad}'\n"
        ));
        assert!(refused.is_err(), "{bad:?} must not reach a command line");
    }

    // Omitting it stays the default: nothing is added anywhere.
    let plain = Config::parse(&format!(
        "{base}[agents.codex-main]\nprovider='codex'\nprofile_ref='default'\n"
    ))
    .unwrap();
    assert_eq!(plain.agents["codex-main"].model, None);
}

#[test]
fn unsupported_instance_topologies_are_not_claimed_as_capabilities() {
    let native = "this='runtime'\n[nodes.runtime]\n[agents.main]\nprovider='claude'\nprofile_ref='default'\n[workspaces.demo]\nroot='/project'\nagent={node='runtime',instance='main'}\n";
    assert!(Config::parse(native).is_err());
    assert!(Config::parse(&native.replace("provider='claude'", "provider='codex'")).is_err());
    let hybrid = RUNTIME.replace("[nodes.runtime]", "[nodes.runtime]\nsmb_user='fixture'")
        .replace("[workspaces.demo]", "[workspaces.demo]\nbackend='hybrid-smb'\nshare='fixture'\nruntime_root='/runtime/build'\nmount_mode='coherence'");
    assert!(Config::parse(&hybrid).is_err());
    for provider in AgentProvider::ALL {
        let caps = provider.instance_capabilities();
        assert!(caps.ssh_mcp && caps.print && caps.interactive);
        assert!(!caps.native_colocated);
    }
}

#[test]
fn preview_preserves_comments_other_workspaces_and_disk_and_is_idempotent() {
    let f = Fixture::new();
    let path = f.0.join("config.toml");
    std::fs::write(&path, LEGACY).unwrap();
    let edit = ccnm_core::configedit::Edit::open(&path).unwrap();
    let preview = edit
        .preview_instance("demo", &reference("claude-main"))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), LEGACY);
    assert!(preview.as_toml().contains("retain selector explanation"));
    assert!(
        preview
            .as_toml()
            .contains("Keep this selector's leading explanation too.")
    );
    let candidate = Config::parse(preview.as_toml()).unwrap();
    assert_eq!(
        candidate.instance_reference("demo").unwrap(),
        reference("claude-main")
    );
    assert_eq!(
        candidate.workspaces["untouched"],
        Config::parse(LEGACY).unwrap().workspaces["untouched"]
    );
    assert_eq!(
        edit.preview_instance("demo", &reference("claude-main"))
            .unwrap()
            .as_toml(),
        preview.as_toml()
    );
    let next = f.0.join("candidate.toml");
    std::fs::write(&next, preview.as_toml()).unwrap();
    let next_edit = ccnm_core::configedit::Edit::open(&next).unwrap();
    assert_eq!(
        next_edit
            .preview_instance("demo", &reference("claude-main"))
            .unwrap()
            .as_toml(),
        preview.as_toml()
    );
    assert!(
        next_edit
            .preview_instance("demo", &reference("codex-main"))
            .is_err()
    );
}

#[test]
fn preview_refuses_legacy_custom_security_semantics_without_writing() {
    let f = Fixture::new();
    let path = f.0.join("config.toml");
    for text in [
        LEGACY.replace(
            "ssh = \"agent-alias\"",
            "ssh = \"agent-alias\"\nclaude_config_dir='/private/legacy'",
        ),
        LEGACY.replace(
            "root = \"/runtime/project\"",
            "root = \"/runtime/project\"\nclaude_permission_mode='plan'",
        ),
    ] {
        std::fs::write(&path, &text).unwrap();
        let edit = ccnm_core::configedit::Edit::open(&path).unwrap();
        assert!(
            edit.preview_instance("demo", &reference("claude-main"))
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn profiles_cannot_alias_each_other_or_the_existing_default_login() {
    let duplicate = "[profiles.a]\nprovider='claude'\ndirectory='/same'\n[profiles.b]\nprovider='codex'\ndirectory='/same'";
    assert!(AgentProfiles::parse(duplicate).is_err());
    let profiles = AgentProfiles::parse(
        "[profiles.extra]\nprovider='codex'\ndirectory='/agent/.config/ccnm/agents/codex'",
    )
    .unwrap();
    let agent =
        Config::parse(&AGENT.replace("profile_ref = \"default\"", "profile_ref = \"extra\""))
            .unwrap();
    assert!(
        agent
            .resolve_instance(
                &reference("codex-main"),
                &profiles,
                Path::new("/agent"),
                None
            )
            .is_err()
    );
}

#[test]
fn root_authority_is_not_a_second_workspace_registry_on_the_agent() {
    let copy = format!(
        "{AGENT}\n[workspaces.demo]\nroot='/runtime/project'\nagent={{node='worker',instance='codex-main'}}"
    );
    assert!(Config::parse(&copy).is_err());
    let (runtime, _, binding) = bound();
    let agent = Config::parse(&AGENT.replace("runtime_node = \"runtime\"", "")).unwrap();
    // A CLI delegation default is not required for an incoming configured Runtime.
    agent
        .resolve_bound_instance(
            &binding,
            &AgentProfiles::default(),
            Path::new("/agent"),
            None,
        )
        .unwrap();
    assert!(agent.instance_reference("demo").is_err());
    assert!(
        runtime
            .resolve_instance_local(&reference("codex-main"))
            .is_err()
    );
}

#[test]
fn session_identity_requires_version_three_and_a_verified_remote_transport() {
    use ccnm_core::{
        protocol::payload,
        session::{Dir, Mode, Spec},
    };
    let (_, _, binding) = bound();
    let f = Fixture::new();
    let spec = Spec {
        runtime_node: Some("runtime".into()),
        protocol: 3,
        agent_identity: Some(binding.agent.clone()),
        provider: AgentProvider::Codex,
        id: "fixture".into(),
        workspace: binding.workspace,
        root: binding.root,
        runtime: Some(ccnm_core::session::RuntimeLink {
            alias: "runtime-alias".into(),
            ccnm_bin: "/runtime/ccnm".into(),
        }),
        provider_config_dir: None,
        permission_mode: Default::default(),
        mode: Mode::Print {
            prompt: "not executed".into(),
        },
        timeout_secs: 10,
        cwd: f.0.clone(),
        codex_exec_server: false,
        agent_tools: Default::default(),
    };
    let dir = Dir::at(f.0.join("record"));
    std::fs::create_dir(dir.path()).unwrap();
    let json = serde_json::to_vec(&spec).unwrap();
    std::fs::write(dir.meta(), &json).unwrap();
    let loaded = ccnm_core::session::load(&dir).unwrap();
    assert_eq!(loaded, spec);
    loaded.check_agent_binding(&binding.agent).unwrap();
    let mut wrong = binding.agent.clone();
    wrong.instance = "other".into();
    assert!(loaded.check_agent_binding(&wrong).is_err());
    assert!(ccnm_core::session::create(&f.0.join("state"), &spec, None).is_err());
    assert!(!f.0.join("state").exists());
    assert!(ccnm_core::session::transport::command(&spec).is_ok());
    let mut wrong = spec.clone();
    wrong.protocol = 2;
    assert!(payload::decode_json::<Spec>(&serde_json::to_vec(&wrong).unwrap()).is_err());
    wrong.protocol = 3;
    wrong.provider = AgentProvider::Claude;
    std::fs::write(dir.meta(), serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert!(ccnm_core::session::load(&dir).is_err());
    wrong.provider = AgentProvider::Codex;
    wrong.provider_config_dir = Some("/private/injected".into());
    assert!(wrong.validate_identity().is_err());
    wrong.provider_config_dir = None;
    wrong.runtime = None;
    assert!(wrong.validate_identity().is_err());
    #[derive(serde::Deserialize)]
    struct LegacyVersion {
        protocol: u32,
    }
    impl payload::Protocol for LegacyVersion {
        fn protocol(&self) -> u32 {
            self.protocol
        }
    }
    assert!(
        payload::decode_json::<LegacyVersion>(&json).is_err(),
        "old peer rejects the record rather than dropping identity"
    );
}

#[test]
fn instance_identity_cannot_be_smuggled_into_legacy_run_requests() {
    let request = serde_json::json!({"protocol":1,"provider":"claude","workspace":"demo","runtime_node":"runtime","root":"/project","claude_config_dir":null,"permission_mode":"acceptEdits","prompt":"never sent","timeout_secs":10,"agent_identity":{"node":"worker","instance":"codex-main","provider":"codex","profile_ref":"default"}});
    assert!(
        serde_json::from_value::<ccnm_core::protocol::run::RunRequest>(request.clone()).is_err()
    );
    let mut start = request;
    start.as_object_mut().unwrap().remove("timeout_secs");
    assert!(serde_json::from_value::<ccnm_core::protocol::run::StartRequest>(start).is_err());
    let identity = serde_json::json!({"node":"worker","instance":"codex-main","provider":"codex","profile_ref":"default"});
    let supervise = serde_json::json!({"protocol":1,"session_dir":"/state","claude_bin":"/agent/claude","agent_identity":identity});
    assert!(serde_json::from_value::<ccnm_core::session::SuperviseRequest>(supervise).is_err());
    let transport =
        serde_json::json!({"protocol":2,"session_dir":"/state","agent_identity":identity});
    assert!(serde_json::from_value::<ccnm_core::session::transport::Request>(transport).is_err());
    let controller = serde_json::json!({"protocol":1,"body":{"request":"start","session_dir":"/state","agent_identity":identity}});
    assert!(serde_json::from_value::<ccnm_core::controller::Request>(controller).is_err());
    let mcp = serde_json::json!({"protocol":1,"workspace":"demo","root":"/project","session":"s","policy":"coding","agent_identity":identity});
    assert!(serde_json::from_value::<ccnm_core::protocol::mcp::ServePayload>(mcp).is_err());
    let stop = serde_json::json!({"protocol":1,"workspace":"demo","agent_identity":identity});
    assert!(serde_json::from_value::<ccnm_core::protocol::run::StopRequest>(stop).is_err());
}

#[test]
fn local_profile_file_uses_agent_xdg_not_ccnm_config_and_preserves_codex_login() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    if let Some(root) = std::env::var_os("CCNM_P2_PROFILE_TEST") {
        let root = PathBuf::from(root);
        let agent = Config::parse(&AGENT.replacen(
            "profile_ref = \"default\"",
            "profile_ref = \"claude-extra\"",
            1,
        ))
        .unwrap();
        if std::env::var_os("CCNM_P2_EXPECT_REFUSAL").is_some() {
            let error = agent
                .resolve_instance_local(&reference("claude-main"))
                .err()
                .unwrap();
            assert!(!error.message().contains("private-selected"));
        } else {
            let claude = agent
                .resolve_instance_local(&reference("claude-main"))
                .unwrap();
            assert_eq!(claude.profile().directory(), root.join("private-selected"));
            let codex = agent
                .resolve_instance_local(&reference("codex-main"))
                .unwrap();
            assert_eq!(
                codex.profile().directory(),
                root.join("xdg/ccnm/agents/codex")
            );
        }
        return;
    }
    let f = Fixture::new();
    let config_dir = f.0.join("xdg/ccnm");
    let codex_home = config_dir.join("agents/codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let auth = codex_home.join("auth.json");
    std::fs::write(&auth, "SYNTHETIC_EXISTING_LOGIN_DO_NOT_COPY").unwrap();
    std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600)).unwrap();
    let profiles = config_dir.join("profiles.toml");
    std::fs::write(
        &profiles,
        format!(
            "[profiles.claude-extra]\nprovider='claude'\ndirectory='{}'",
            f.0.join("private-selected").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&profiles, std::fs::Permissions::from_mode(0o600)).unwrap();
    let decoy = f.0.join("decoy");
    std::fs::create_dir(&decoy).unwrap();
    std::fs::write(decoy.join("profiles.toml"), "not a profile registry").unwrap();
    let run = |refused: bool| {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "local_profile_file_uses_agent_xdg_not_ccnm_config_and_preserves_codex_login",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &f.0)
        .env("XDG_CONFIG_HOME", f.0.join("xdg"))
        .env("CCNM_CONFIG", decoy.join("config.toml"))
        .env("CODEX_HOME", f.0.join("wrong-personal-home"))
        .env("CCNM_P2_PROFILE_TEST", &f.0);
        if refused {
            cmd.env("CCNM_P2_EXPECT_REFUSAL", "1");
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(false);
    std::fs::set_permissions(&profiles, std::fs::Permissions::from_mode(0o644)).unwrap();
    run(true);
    std::fs::rename(&profiles, config_dir.join("saved-profiles.toml")).unwrap();
    symlink("saved-profiles.toml", &profiles).unwrap();
    run(true);
    assert_eq!(
        std::fs::read_to_string(auth).unwrap(),
        "SYNTHETIC_EXISTING_LOGIN_DO_NOT_COPY"
    );
    assert!(!f.0.join("wrong-personal-home").exists());
    assert!(!f.0.join("private-selected").exists());
}
