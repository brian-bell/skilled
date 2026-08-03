use skilled::{AgentKind, agents::adapter};
use std::path::Path;

#[test]
fn adapters_record_the_documented_global_discovery_conventions() {
    let claude = adapter(AgentKind::ClaudeCode);
    assert_eq!(claude.native_skill_root(), ".claude/skills");
    assert!(claude.compatibility_skill_roots().is_empty());
    assert_eq!(
        claude.documentation().url(),
        "https://code.claude.com/docs/en/slash-commands"
    );

    let codex = adapter(AgentKind::Codex);
    assert_eq!(codex.native_skill_root(), ".agents/skills");
    assert_eq!(codex.configuration_path(), Some(".codex/config.toml"));
    assert_eq!(
        codex.documentation().url(),
        "https://developers.openai.com/codex/skills"
    );

    let opencode = adapter(AgentKind::OpenCode);
    assert_eq!(opencode.native_skill_root(), ".config/opencode/skills");
    assert_eq!(
        opencode.compatibility_skill_roots(),
        [".agents/skills", ".claude/skills"]
    );
    assert_eq!(
        opencode.documentation().url(),
        "https://opencode.ai/docs/skills"
    );

    for kind in AgentKind::ALL {
        assert_eq!(adapter(kind).documentation().snapshot_date(), "2026-08-02");
    }
}

#[test]
fn adapters_own_source_catalog_compatibility() {
    let claude = adapter(AgentKind::ClaudeCode);
    assert!(claude.supports_source_catalog(Path::new("catalogs/claude-code/skills")));
    assert!(claude.supports_source_catalog(Path::new(".claude/skills")));
    assert!(!claude.supports_source_catalog(Path::new(".agents/skills")));

    let codex = adapter(AgentKind::Codex);
    assert!(codex.supports_source_catalog(Path::new("catalogs/codex/skills")));
    assert!(codex.supports_source_catalog(Path::new(".agents/skills")));
    assert!(!codex.supports_source_catalog(Path::new(".claude/skills")));

    let opencode = adapter(AgentKind::OpenCode);
    assert!(opencode.supports_source_catalog(Path::new("catalogs/opencode/skills")));
    assert!(opencode.supports_source_catalog(Path::new(".config/opencode/skills")));
    assert!(opencode.supports_source_catalog(Path::new(".agents/skills")));
    assert!(opencode.supports_source_catalog(Path::new(".claude/skills")));
}
