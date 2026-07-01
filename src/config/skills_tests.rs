use super::*;
use tempfile::TempDir;

fn create_skill_file(dir: &Path, name: &str, description: &str) -> PathBuf {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    let content = format!(
        "---\nname: {name}\ndescription: |\n  {description}\n---\n\n# {name}\n\nSkill content here.\n"
    );
    let path = skill_dir.join("SKILL.md");
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn skill_parse_valid() {
    let tmp = TempDir::new().unwrap();
    let path = create_skill_file(tmp.path(), "test-skill", "A test skill description");

    let skill = Skill::parse(&path, SkillScope::Project).unwrap();
    assert_eq!(skill.name, "test-skill");
    assert_eq!(skill.description, "A test skill description");
    assert_eq!(skill.scope, SkillScope::Project);
    assert_eq!(skill.location, path);
    assert_eq!(skill.base_directory, path.parent().unwrap());
}

#[test]
fn skill_parse_does_not_decode_body() {
    let tmp = TempDir::new().unwrap();
    let skill_dir = tmp.path().join("lazy-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let path = skill_dir.join("SKILL.md");
    std::fs::write(
        &path,
        b"---\nname: lazy-skill\ndescription: Metadata only\n---\n\n\xFF",
    )
    .unwrap();

    let skill = Skill::parse(&path, SkillScope::Project).unwrap();

    assert_eq!(skill.name, "lazy-skill");
    assert_eq!(skill.description, "Metadata only");
}

#[test]
fn skill_parse_multiline_description() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let content = r"---
name: my-skill
description: |
  First line of description
  Second line of description
---

# My Skill
";
    let path = dir.join("SKILL.md");
    std::fs::write(&path, content).unwrap();

    let skill = Skill::parse(&path, SkillScope::Project).unwrap();
    assert_eq!(skill.name, "my-skill");
    assert!(skill.description.contains("First line"));
    assert!(skill.description.contains("Second line"));
}

#[test]
fn skill_parse_multiline_description_with_indented_separator_text() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("separator-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let content = r"---
name: separator-skill
description: |
  First line
  ---
  Last line
---

# Separator Skill
";
    let path = dir.join("SKILL.md");
    std::fs::write(&path, content).unwrap();

    let skill = Skill::parse(&path, SkillScope::Project).unwrap();

    assert!(skill.description.contains("---"));
    assert!(skill.description.contains("Last line"));
}

#[test]
fn skill_parse_missing_name() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("bad-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let content = r"---
description: Something
---
";
    let path = dir.join("SKILL.md");
    std::fs::write(&path, content).unwrap();

    let result = Skill::parse(&path, SkillScope::Project);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.level, DiagnosticLevel::Error);
    assert!(err.message.contains("name"));
}

#[test]
fn skill_parse_missing_description() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("bad-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let content = r"---
name: something
---
";
    let path = dir.join("SKILL.md");
    std::fs::write(&path, content).unwrap();

    let result = Skill::parse(&path, SkillScope::Project);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.level, DiagnosticLevel::Error);
    assert!(err.message.contains("description"));
}

#[test]
fn skill_parse_missing_frontmatter() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("bad-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let content = "# Just markdown\n";
    let path = dir.join("SKILL.md");
    std::fs::write(&path, content).unwrap();

    let result = Skill::parse(&path, SkillScope::Project);
    assert!(result.is_err());
}

#[test]
fn skill_load_body() {
    let tmp = TempDir::new().unwrap();
    let path = create_skill_file(tmp.path(), "body-test", "A test");

    let skill = Skill::parse(&path, SkillScope::Project).unwrap();
    let body = skill.load_body().unwrap();
    assert!(body.contains("# body-test"));
    assert!(body.contains("Skill content here"));
    assert!(!body.contains("---"));
}

#[test]
fn catalog_get_skill_by_location() {
    let tmp = TempDir::new().unwrap();
    let path = create_skill_file(tmp.path(), "loc-test", "A test");

    let mut catalog = SkillCatalog::empty();
    let skill = Skill::parse(&path, SkillScope::Project).unwrap();
    catalog.skills.push(skill);

    assert!(catalog.get_skill_by_location(&path).is_some());
    assert!(
        catalog
            .get_skill_by_location(Path::new("/nonexistent"))
            .is_none()
    );
}

#[test]
fn catalog_filter_to() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "skill-a".to_string(),
        description: "A".to_string(),
        location: PathBuf::from("/a"),
        base_directory: PathBuf::from("/"),
        scope: SkillScope::Project,
    });
    catalog.skills.push(Skill {
        name: "skill-b".to_string(),
        description: "B".to_string(),
        location: PathBuf::from("/b"),
        base_directory: PathBuf::from("/"),
        scope: SkillScope::Project,
    });
    catalog.skills.push(Skill {
        name: "skill-c".to_string(),
        description: "C".to_string(),
        location: PathBuf::from("/c"),
        base_directory: PathBuf::from("/"),
        scope: SkillScope::Project,
    });

    catalog.filter_to(&["skill-a".to_string(), "skill-c".to_string()]);
    assert_eq!(catalog.skills.len(), 2);
    assert_eq!(catalog.skills[0].name, "skill-a");
    assert_eq!(catalog.skills[1].name, "skill-c");
}

#[test]
fn catalog_to_prompt_xml() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "debugging".to_string(),
        description: "How to debug things".to_string(),
        location: PathBuf::from("/path/to/debugging/SKILL.md"),
        base_directory: PathBuf::from("/path/to/debugging"),
        scope: SkillScope::Project,
    });

    let xml = catalog.to_prompt_xml();
    assert!(xml.contains("<available_skills>"));
    assert!(xml.contains("<name>debugging</name>"));
    assert!(xml.contains("<description>How to debug things</description>"));
    assert!(xml.contains("<location>/path/to/debugging/SKILL.md</location>"));
    assert!(xml.contains("</available_skills>"));
}

#[test]
fn snapshot_skill_catalog_single_skill_xml() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "debugging".to_string(),
        description: "How to debug Rust programs".to_string(),
        location: PathBuf::from("/project/.agents/skills/debugging/SKILL.md"),
        base_directory: PathBuf::from("/project/.agents/skills/debugging"),
        scope: SkillScope::Project,
    });

    insta::assert_snapshot!("skill_catalog_single_skill_xml", catalog.to_prompt_xml());
}

#[test]
fn catalog_to_prompt_xml_empty() {
    let catalog = SkillCatalog::empty();
    assert_eq!(catalog.to_prompt_xml(), "");
}

#[test]
fn catalog_to_prompt_xml_escapes() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "test <&>".to_string(),
        description: "Desc with <tag> & \"quotes\"".to_string(),
        location: PathBuf::from("/a"),
        base_directory: PathBuf::from("/"),
        scope: SkillScope::Project,
    });

    let xml = catalog.to_prompt_xml();
    assert!(xml.contains("&lt;"));
    assert!(xml.contains("&gt;"));
    assert!(xml.contains("&amp;"));
    assert!(xml.contains("&quot;"));
}

#[test]
fn discover_skills_finds_project_skills() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join(".agents").join("skills");
    std::fs::create_dir_all(&agents_dir).unwrap();
    create_skill_file(&agents_dir, "skill-a", "First skill");
    create_skill_file(&agents_dir, "skill-b", "Second skill");

    let catalog = discover_skills(tmp.path());
    let project_skills: Vec<_> = catalog
        .skills
        .iter()
        .filter(|s| s.scope == SkillScope::Project)
        .collect();
    assert_eq!(project_skills.len(), 2);
    let names: Vec<_> = project_skills.iter().map(|s| s.name.clone()).collect();
    assert!(names.contains(&"skill-a".to_string()));
    assert!(names.contains(&"skill-b".to_string()));
}

#[test]
fn discover_skills_finds_configured_skills() {
    let home = TempDir::new().unwrap();
    temp_env::with_var("HOME", Some(home.path()), || {
        let tmp = TempDir::new().unwrap();
        let configured_dir = TempDir::new().unwrap();
        create_skill_file(configured_dir.path(), "team-skill", "Team skill");

        let catalog =
            discover_skills_with_paths(tmp.path(), &[configured_dir.path().to_path_buf()]);

        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "team-skill");
        assert_eq!(catalog.skills[0].scope, SkillScope::Configured);
    });
}

#[test]
fn discover_skills_project_shadows_configured_skills() {
    let home = TempDir::new().unwrap();
    temp_env::with_var("HOME", Some(home.path()), || {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".agents").join("skills");
        std::fs::create_dir_all(&agents_dir).unwrap();
        create_skill_file(&agents_dir, "shared-skill", "Project skill");

        let configured_dir = TempDir::new().unwrap();
        create_skill_file(configured_dir.path(), "shared-skill", "Configured skill");

        let catalog =
            discover_skills_with_paths(tmp.path(), &[configured_dir.path().to_path_buf()]);

        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].scope, SkillScope::Project);
        assert!(
            catalog
                .diagnostics
                .iter()
                .any(|d| d.message.contains("shadowed by project skill"))
        );
    });
}

#[test]
fn parse_skill_path_list_splits_paths_and_expands_home() {
    let home = TempDir::new().unwrap();
    temp_env::with_var("HOME", Some(home.path()), || {
        let separator = if cfg!(windows) { ";" } else { ":" };
        let paths = parse_skill_path_list(&format!("~/my-skills{separator}/shared/team-skills"));

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], home.path().join("my-skills"));
        assert_eq!(paths[1], PathBuf::from("/shared/team-skills"));
    });
}

#[test]
fn discover_skills_skips_excluded_dirs() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join(".agents").join("skills");
    std::fs::create_dir_all(&agents_dir).unwrap();

    // Create a skill inside .git (should be skipped)
    let git_dir = agents_dir.join(".git").join("nested");
    std::fs::create_dir_all(&git_dir).unwrap();
    create_skill_file(&git_dir, "git-skill", "Should not appear");

    // Create a normal skill
    create_skill_file(&agents_dir, "normal-skill", "Should appear");

    let catalog = discover_skills(tmp.path());
    let project_skills: Vec<_> = catalog
        .skills
        .iter()
        .filter(|s| s.scope == SkillScope::Project)
        .collect();
    assert_eq!(project_skills.len(), 1);
    assert_eq!(project_skills[0].name, "normal-skill");
}

#[test]
fn skill_config_apply_disabled() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "a".to_string(),
        description: "A".to_string(),
        location: PathBuf::from("/a"),
        base_directory: PathBuf::from("/"),
        scope: SkillScope::Project,
    });

    let result = SkillConfig::Disabled.apply(catalog);
    assert!(result.skills.is_empty());
}

#[test]
fn skill_config_apply_only() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "a".to_string(),
        description: "A".to_string(),
        location: PathBuf::from("/a"),
        base_directory: PathBuf::from("/"),
        scope: SkillScope::Project,
    });
    catalog.skills.push(Skill {
        name: "b".to_string(),
        description: "B".to_string(),
        location: PathBuf::from("/b"),
        base_directory: PathBuf::from("/"),
        scope: SkillScope::Project,
    });

    let result = SkillConfig::Only(vec!["a".to_string()]).apply(catalog);
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "a");
}

#[test]
fn discover_skills_respects_max_depth() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join(".agents").join("skills");
    std::fs::create_dir_all(&agents_dir).unwrap();

    // Create nested structure deeper than MAX_DEPTH
    let mut deep_dir = agents_dir.clone();
    for i in 0..=MAX_DEPTH + 2 {
        deep_dir = deep_dir.join(format!("level{i}"));
        std::fs::create_dir_all(&deep_dir).unwrap();
    }
    create_skill_file(&deep_dir, "deep-skill", "Too deep");

    // Create a shallow skill
    create_skill_file(&agents_dir, "shallow-skill", "Should appear");

    let catalog = discover_skills(tmp.path());
    let project_skills: Vec<_> = catalog
        .skills
        .iter()
        .filter(|s| s.scope == SkillScope::Project)
        .collect();
    assert_eq!(project_skills.len(), 1);
    assert_eq!(project_skills[0].name, "shallow-skill");
}
