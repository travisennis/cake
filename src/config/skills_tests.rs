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
fn skill_parse_quoted_colon_in_description() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("quoted-colon-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let content = r#"---
name: quoted-colon-skill
description: "A skill: with a colon"
---
"#;
    let path = dir.join("SKILL.md");
    std::fs::write(&path, content).unwrap();

    let skill = Skill::parse(&path, SkillScope::Project).unwrap();

    assert_eq!(skill.description, "A skill: with a colon");
}

#[test]
fn skill_parse_unquoted_colon_in_description_uses_fallback() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("unquoted-colon-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let content = r"---
name: unquoted-colon-skill
description: A skill: with a colon
---
";
    let path = dir.join("SKILL.md");
    std::fs::write(&path, content).unwrap();

    let skill = Skill::parse(&path, SkillScope::Project).unwrap();

    assert_eq!(skill.description, "A skill: with a colon");
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
fn catalog_size_report_measures_rendered_xml_without_mutation() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "routing".to_string(),
        description: "é<&".to_string(),
        location: PathBuf::from("/skills/routing/SKILL.md"),
        base_directory: PathBuf::from("/skills/routing"),
        scope: SkillScope::Project,
    });
    let xml = catalog.to_prompt_xml();
    let report = catalog.size_report(8000, 3);
    let characters = xml.chars().count();
    assert!(report.contains(&format!("{} bytes, {characters} characters", xml.len())));
    assert!(xml.contains("é&lt;&amp;"));
    assert!(report.contains("3 description characters: routing\n"));
    assert!(!report.contains("WARNING"));
    assert_eq!(catalog.to_prompt_xml(), xml);

    let report = catalog.size_report(characters - 1, 2);
    assert!(report.contains("WARNING: rendered catalog exceeds"));
    assert!(report.contains("routing WARNING"));
    assert!(report.contains("skills.only"));
    assert_eq!(catalog.to_prompt_xml(), xml);
}

#[test]
fn catalog_size_report_empty_and_filtered() {
    let dir = tempfile::tempdir().unwrap();
    create_skill_file(dir.path(), "example", "A routing description");
    let mut catalog = discover_skills_inner(dir.path(), &[dir.path().to_path_buf()], None);
    assert_eq!(catalog.skills.len(), 1);
    catalog.filter_to(&["absent".to_string()]);
    assert_eq!(
        catalog.size_report(0, 0),
        SkillCatalog::empty().size_report(0, 0)
    );
    assert!(
        catalog
            .size_report(0, 0)
            .contains("0 skills, 0 bytes, 0 characters, ~0 tokens")
    );
    assert!(!catalog.size_report(0, 0).contains("WARNING"));
}

#[test]
fn catalog_size_report_orders_descriptions_largest_first() {
    let dir = tempfile::tempdir().unwrap();
    create_skill_file(dir.path(), "short", "small");
    create_skill_file(dir.path(), "large", "a longer routing description");
    let catalog = discover_skills_inner(dir.path(), &[dir.path().to_path_buf()], None);
    let report = catalog.size_report(1, 10);
    assert!(report.find(": large WARNING").unwrap() < report.find(": short\n").unwrap());
    assert!(report.contains("WARNING: rendered catalog exceeds"));
}

#[test]
fn skill_scope_labels_are_stable() {
    assert_eq!(SkillScope::Project.label(), "project");
    assert_eq!(SkillScope::Configured.label(), "configured");
    assert_eq!(SkillScope::User.label(), "user");
}

#[test]
fn diagnostic_level_labels_are_stable() {
    assert_eq!(DiagnosticLevel::Warning.label(), "warning");
    assert_eq!(DiagnosticLevel::Error.label(), "error");
}

#[test]
fn skill_catalog_budget_uses_context_window_and_clamps() {
    assert_eq!(
        SkillCatalogBudget::from_context_window(None),
        SkillCatalogBudget::defaults()
    );
    assert_eq!(
        SkillCatalogBudget::from_context_window(Some(200_000)).catalog_characters,
        16_000
    );
    assert_eq!(
        SkillCatalogBudget::from_context_window(Some(1_000)).catalog_characters,
        MIN_CONTEXT_SKILL_CATALOG_CHARACTERS
    );
    assert_eq!(
        SkillCatalogBudget::from_context_window(Some(u32::MAX)).catalog_characters,
        MAX_CONTEXT_SKILL_CATALOG_CHARACTERS
    );
    assert_eq!(
        SkillCatalogBudget::from_context_window(Some(200_000)).description_characters,
        DEFAULT_SKILL_DESCRIPTION_CHARACTERS
    );
}

#[test]
fn skill_catalog_budget_explicit_values_override_context_default() {
    let budget = SkillCatalogBudget::resolve(Some(200_000), Some(1_234), Some(56));
    assert_eq!(
        budget,
        SkillCatalogBudget::new(1_234, 56),
        "explicit settings must win over the context-relative default"
    );

    let budget = SkillCatalogBudget::resolve(Some(200_000), None, Some(56));
    assert_eq!(budget.catalog_characters, 16_000);
    assert_eq!(budget.description_characters, 56);
}

#[test]
fn shorten_description_uses_a_word_boundary_and_ellipsis() {
    assert_eq!(shorten_description("abc", 3), "abc");
    assert_eq!(shorten_description("one two three four", 10), "one two…");
    assert_eq!(shorten_description("abc", 1), "…");
    assert_eq!(shorten_description("abc", 0), "");
    assert_eq!(shorten_description("ééé", 2), "é…");
    assert_eq!(shorten_description("abcdef", 4), "abc…");
}

#[test]
fn budgeted_catalog_shortens_descriptions_before_omitting_entries() {
    let mut catalog = SkillCatalog::empty();
    for (name, description) in [
        ("one", "a description that is much too long"),
        ("two", "another description that is much too long"),
        ("three", "a third description that is much too long"),
    ] {
        catalog.skills.push(Skill {
            name: name.to_string(),
            description: description.to_string(),
            location: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_directory: PathBuf::from(format!("/skills/{name}")),
            scope: SkillScope::Project,
        });
    }
    let original = catalog.to_prompt_xml();
    let budget = SkillCatalogBudget::new(300, 12);

    let render = catalog.to_prompt_xml_with_budget(budget, true);

    assert!(!render.over_budget);
    assert!(render.metrics().characters <= budget.catalog_characters);
    assert!(render.shortened_descriptions > 0);
    assert!(!render.omitted_skills.is_empty());
    assert!(render.xml.contains("<omitted_skills"));
    assert_eq!(catalog.to_prompt_xml(), original);
}

#[test]
fn budgeted_catalog_does_not_omit_without_a_search_path() {
    let mut catalog = SkillCatalog::empty();
    for name in ["one", "two", "three"] {
        catalog.skills.push(Skill {
            name: name.to_string(),
            description: "a description that is much too long".to_string(),
            location: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_directory: PathBuf::from(format!("/skills/{name}")),
            scope: SkillScope::Project,
        });
    }
    let budget = SkillCatalogBudget::new(1, 12);

    let render = catalog.to_prompt_xml_with_budget(budget, false);

    assert!(render.over_budget);
    assert!(render.omitted_skills.is_empty());
    assert!(!render.xml.contains("<omitted_skills"));
    assert!(render.xml.contains("<name>one</name>"));
    assert!(render.xml.contains("<name>three</name>"));
}

#[test]
fn budgeted_catalog_retains_discovery_prefix_when_omitting() {
    let mut catalog = SkillCatalog::empty();
    for (index, name) in ["project", "configured", "user"].into_iter().enumerate() {
        catalog.skills.push(Skill {
            name: name.to_string(),
            description: "a long routing description that must be shortened".to_string(),
            location: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_directory: PathBuf::from(format!("/skills/{name}")),
            scope: match index {
                0 => SkillScope::Project,
                1 => SkillScope::Configured,
                _ => SkillScope::User,
            },
        });
    }

    let render = catalog.to_prompt_xml_with_budget(SkillCatalogBudget::new(350, 10), true);

    assert!(render.xml.contains("<name>project</name>"));
    assert!(render.omitted_skills.contains(&"user".to_string()));
    assert!(
        !render.omitted_skills.contains(&"project".to_string()),
        "higher-precedence skills must survive omission"
    );
}

#[test]
fn budgeted_catalog_retains_first_skill_when_budget_cannot_fit_an_entry() {
    let mut catalog = SkillCatalog::empty();
    for index in 0..12 {
        let name = format!("skill-{index}");
        catalog.skills.push(Skill {
            name: name.clone(),
            description: "a long routing description that must be shortened".to_string(),
            location: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_directory: PathBuf::from(format!("/skills/{name}")),
            scope: SkillScope::User,
        });
    }

    let render = catalog.to_prompt_xml_with_budget(SkillCatalogBudget::new(1, 10), true);

    assert!(render.over_budget);
    assert_eq!(render.omitted_skills.len(), 11);
    assert!(!render.omitted_skills.contains(&"skill-0".to_string()));
    assert!(render.xml.contains("<name>skill-0</name>"));
    assert!(render.xml.contains("<omitted_skills count=\"11\">"));
    assert!(render.xml.contains("<more count=\"3\"/>"));
    assert!(render.xml.contains("<available_skills>"));
}

#[test]
fn budgeted_catalog_retains_one_skill_when_budget_is_just_below_an_entry() {
    let mut catalog = SkillCatalog::empty();
    for name in ["one", "two"] {
        catalog.skills.push(Skill {
            name: name.to_string(),
            description: "short description".to_string(),
            location: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_directory: PathBuf::from(format!("/skills/{name}")),
            scope: SkillScope::Project,
        });
    }
    let mut solo = SkillCatalog::empty();
    solo.skills.push(catalog.skills[0].clone());
    let one_entry_characters = solo.to_prompt_xml().chars().count();

    let render = catalog
        .to_prompt_xml_with_budget(SkillCatalogBudget::new(one_entry_characters - 1, 100), true);

    assert!(render.over_budget);
    assert!(render.xml.contains("<name>one</name>"));
    assert_eq!(render.omitted_skills, vec!["two".to_string()]);
}

#[test]
fn budgeted_catalog_fits_one_entry_and_omits_the_rest() {
    let mut catalog = SkillCatalog::empty();
    for name in ["one", "two"] {
        catalog.skills.push(Skill {
            name: name.to_string(),
            description: "short description".to_string(),
            location: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_directory: PathBuf::from(format!("/skills/{name}")),
            scope: SkillScope::Project,
        });
    }
    let omitted = vec!["two".to_string()];
    let expected = render_skill_catalog(
        std::iter::once((&catalog.skills[0], catalog.skills[0].description.as_str())),
        &omitted,
    );

    let render = catalog
        .to_prompt_xml_with_budget(SkillCatalogBudget::new(expected.chars().count(), 100), true);

    assert!(!render.over_budget);
    assert_eq!(render.xml, expected);
    assert_eq!(render.omitted_skills, omitted);
    assert!(render.xml.contains("<name>one</name>"));
    assert!(!render.xml.contains("<name>two</name>"));
}

#[test]
fn budgeted_catalog_omission_is_deterministic() {
    let mut catalog = SkillCatalog::empty();
    for name in ["one", "two", "three", "four"] {
        catalog.skills.push(Skill {
            name: name.to_string(),
            description: "a long routing description that must be shortened".to_string(),
            location: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_directory: PathBuf::from(format!("/skills/{name}")),
            scope: SkillScope::Project,
        });
    }
    let budget = SkillCatalogBudget::new(300, 10);
    let expected = catalog.to_prompt_xml_with_budget(budget, true);

    for _ in 0..5 {
        assert_eq!(catalog.to_prompt_xml_with_budget(budget, true), expected);
    }
}

#[test]
fn budgeted_catalog_empty_catalog_is_empty() {
    let render =
        SkillCatalog::empty().to_prompt_xml_with_budget(SkillCatalogBudget::new(100, 10), true);
    assert!(render.xml.is_empty());
    assert!(render.omitted_skills.is_empty());
    assert!(!render.over_budget);
}

#[test]
fn catalog_metrics_match_rendered_xml_and_budget_boundaries() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "routing".to_string(),
        description: "é<&".to_string(),
        location: PathBuf::from("/skills/routing/SKILL.md"),
        base_directory: PathBuf::from("/skills/routing"),
        scope: SkillScope::Project,
    });

    let xml = catalog.to_prompt_xml();
    let metrics = catalog.metrics();
    assert_eq!(metrics.bytes, xml.len());
    assert_eq!(metrics.characters, xml.chars().count());
    assert_eq!(metrics.estimated_tokens, metrics.characters.div_ceil(4));
    assert!(!metrics.catalog_over_budget(metrics.characters));
    assert!(metrics.catalog_over_budget(metrics.characters - 1));
    assert_eq!(metrics.catalog_over_budget_by(metrics.characters), 0);
    assert_eq!(metrics.catalog_over_budget_by(metrics.characters - 1), 1);
    assert_eq!(catalog.to_prompt_xml(), xml);
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

    let catalog = discover_skills_inner(tmp.path(), &[], None);
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
    let tmp = TempDir::new().unwrap();
    let configured_dir = TempDir::new().unwrap();
    create_skill_file(configured_dir.path(), "team-skill", "Team skill");

    let catalog = discover_skills_inner(tmp.path(), &[configured_dir.path().to_path_buf()], None);

    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(catalog.skills[0].name, "team-skill");
    assert_eq!(catalog.skills[0].scope, SkillScope::Configured);
}

#[test]
fn discover_skills_project_shadows_configured_skills() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join(".agents").join("skills");
    std::fs::create_dir_all(&agents_dir).unwrap();
    create_skill_file(&agents_dir, "shared-skill", "Project skill");

    let configured_dir = TempDir::new().unwrap();
    create_skill_file(configured_dir.path(), "shared-skill", "Configured skill");

    let catalog = discover_skills_inner(tmp.path(), &[configured_dir.path().to_path_buf()], None);

    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(catalog.skills[0].scope, SkillScope::Project);
    assert!(
        catalog
            .diagnostics
            .iter()
            .any(|d| d.message.contains("shadowed by project skill"))
    );
}

#[test]
fn discover_skills_finds_user_skills() {
    let tmp = TempDir::new().unwrap();
    let user_dir = TempDir::new().unwrap();
    let user_skills_dir = user_dir.path().join(".agents").join("skills");
    create_skill_file(&user_skills_dir, "user-skill", "User skill");

    let catalog = discover_skills_inner(tmp.path(), &[], Some(&user_skills_dir));

    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(catalog.skills[0].name, "user-skill");
    assert_eq!(catalog.skills[0].scope, SkillScope::User);
}

#[test]
fn discover_skills_project_shadows_user_skills() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join(".agents").join("skills");
    std::fs::create_dir_all(&agents_dir).unwrap();
    create_skill_file(&agents_dir, "shared-skill", "Project skill");

    let user_dir = TempDir::new().unwrap();
    let user_skills_dir = user_dir.path().join(".agents").join("skills");
    create_skill_file(&user_skills_dir, "shared-skill", "User skill");

    let catalog = discover_skills_inner(tmp.path(), &[], Some(&user_skills_dir));

    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(catalog.skills[0].scope, SkillScope::Project);
    assert!(
        catalog
            .diagnostics
            .iter()
            .any(|d| d.message.contains("shadowed by higher-precedence skill"))
    );
}

#[test]
fn discover_skills_configured_shadows_user_skills() {
    let tmp = TempDir::new().unwrap();
    let configured_dir = TempDir::new().unwrap();
    create_skill_file(configured_dir.path(), "shared-skill", "Configured skill");

    let user_dir = TempDir::new().unwrap();
    let user_skills_dir = user_dir.path().join(".agents").join("skills");
    create_skill_file(&user_skills_dir, "shared-skill", "User skill");

    let catalog = discover_skills_inner(
        tmp.path(),
        &[configured_dir.path().to_path_buf()],
        Some(&user_skills_dir),
    );

    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(catalog.skills[0].scope, SkillScope::Configured);
    assert!(
        catalog
            .diagnostics
            .iter()
            .any(|d| d.message.contains("shadowed by higher-precedence skill"))
    );
}

#[test]
fn discover_skills_resolves_user_skills_from_home() {
    let home = TempDir::new().unwrap();
    let user_skills_dir = home.path().join(".agents").join("skills");
    create_skill_file(&user_skills_dir, "home-skill", "User skill from home");
    temp_env::with_var("HOME", Some(home.path()), || {
        let tmp = TempDir::new().unwrap();
        let catalog = discover_skills(tmp.path());
        let user_skills: Vec<_> = catalog
            .skills
            .iter()
            .filter(|s| s.scope == SkillScope::User)
            .collect();
        assert_eq!(user_skills.len(), 1);
        assert_eq!(user_skills[0].name, "home-skill");
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

    let catalog = discover_skills_inner(tmp.path(), &[], None);
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

    let catalog = discover_skills_inner(tmp.path(), &[], None);
    let project_skills: Vec<_> = catalog
        .skills
        .iter()
        .filter(|s| s.scope == SkillScope::Project)
        .collect();
    assert_eq!(project_skills.len(), 1);
    assert_eq!(project_skills[0].name, "shallow-skill");
}

#[test]
fn discover_skills_with_paths_missing_dir_emits_warning() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("does-not-exist");

    let catalog = discover_skills_inner(tmp.path(), std::slice::from_ref(&missing), None);

    assert!(catalog.diagnostics.iter().any(|d| {
        d.level == DiagnosticLevel::Warning
            && d.file == missing
            && d.message.contains("does not exist")
    }));
}

#[test]
fn discover_skills_with_paths_empty_dir_emits_warning() {
    let tmp = TempDir::new().unwrap();
    let empty_dir = TempDir::new().unwrap();

    let catalog = discover_skills_inner(tmp.path(), &[empty_dir.path().to_path_buf()], None);

    assert!(catalog.diagnostics.iter().any(|d| {
        d.level == DiagnosticLevel::Warning
            && d.file == empty_dir.path()
            && d.message.contains("resolved to zero skills")
    }));
}

#[test]
fn discover_skills_with_paths_non_empty_dir_does_not_emit_empty_warning() {
    let tmp = TempDir::new().unwrap();
    let configured_dir = TempDir::new().unwrap();
    create_skill_file(configured_dir.path(), "my-skill", "A skill");

    let catalog = discover_skills_inner(tmp.path(), &[configured_dir.path().to_path_buf()], None);

    assert!(
        !catalog
            .diagnostics
            .iter()
            .any(|d| d.message.contains("resolved to zero skills"))
    );
}

#[test]
fn skill_config_apply_only_missing_name_emits_warning() {
    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "existing".to_string(),
        description: "Exists".to_string(),
        location: PathBuf::from("/a"),
        base_directory: PathBuf::from("/"),
        scope: SkillScope::Project,
    });

    let result =
        SkillConfig::Only(vec!["existing".to_string(), "missing".to_string()]).apply(catalog);

    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "existing");
    assert!(result.diagnostics.iter().any(|d| {
        d.message
            .contains("Requested skill 'missing' was not found")
    }));
}

#[test]
fn skill_config_apply_only_all_found_no_diagnostics() {
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

    let result = SkillConfig::Only(vec!["a".to_string(), "b".to_string()]).apply(catalog);

    assert_eq!(result.skills.len(), 2);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("not found"))
    );
}
