//! Skill discovery, parsing, and catalog management for cake.
//!
//! Skills provide specialized instructions for specific tasks. They are discovered
//! from `.agents/skills/` directories at both project and user levels, plus
//! optional configured skill directories.
//!
//! Each skill is defined by a `SKILL.md` file with YAML frontmatter containing
//! metadata (name, description) and markdown body content.

use std::collections::HashSet;
use std::io::BufRead;
use std::path::{Path, PathBuf};

// =============================================================================
// Core Types
// =============================================================================

/// A discovered skill with parsed metadata.
///
/// Skills are loaded from `SKILL.md` files found in skill root subdirectories.
/// The body content is lazy-loaded at activation time to minimize memory usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Skill name (from YAML frontmatter)
    pub name: String,
    /// Skill description (from YAML frontmatter)
    pub description: String,
    /// Absolute path to the SKILL.md file
    pub location: PathBuf,
    /// Parent directory of SKILL.md (base for resolving relative paths)
    pub base_directory: PathBuf,
    /// Scope indicating whether this is a project-level or user-level skill
    pub scope: SkillScope,
}

/// Scope of a skill indicating its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    /// Skill discovered from the project's `.agents/skills/` directory
    Project,
    /// Skill discovered from a directory configured in settings.toml
    Configured,
    /// Skill discovered from the user's `~/.agents/skills/` directory
    User,
}

/// Diagnostic level for skill parsing/loading issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    /// Non-fatal issue (e.g., name mismatch)
    Warning,
    /// Fatal issue that prevents the skill from being loaded
    Error,
}

/// A diagnostic message about a skill.
#[derive(Debug, Clone)]
pub struct SkillDiagnostic {
    /// Severity level
    pub level: DiagnosticLevel,
    /// Human-readable message
    pub message: String,
    /// Path to the file that caused the diagnostic
    pub file: PathBuf,
}

/// Collection of discovered skills with diagnostics.
#[derive(Debug, Clone)]
pub struct SkillCatalog {
    /// Discovered skills (filtered by configuration)
    pub skills: Vec<Skill>,
    /// Diagnostics from discovery/parsing
    pub diagnostics: Vec<SkillDiagnostic>,
}

// =============================================================================
// Skill Parsing
// =============================================================================

/// Parsed YAML frontmatter from a SKILL.md file.
#[derive(Debug, serde::Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

impl Skill {
    /// Parse a SKILL.md file and extract metadata.
    ///
    /// Reads the file, extracts YAML frontmatter between `---` delimiters,
    /// and validates required fields.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if the file cannot be read, has invalid frontmatter,
    /// or is missing required fields.
    pub fn parse(path: &Path, scope: SkillScope) -> Result<Self, SkillDiagnostic> {
        let yaml_text = Self::read_frontmatter(path)?;
        let (name, description) = Self::parse_frontmatter_yaml(&yaml_text, path)?;

        let base_directory = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

        Ok(Self {
            name,
            description,
            location: path.to_path_buf(),
            base_directory,
            scope,
        })
    }

    /// Read only the YAML frontmatter from a skill file.
    fn read_frontmatter(path: &Path) -> Result<String, SkillDiagnostic> {
        let file = std::fs::File::open(path).map_err(|e| SkillDiagnostic {
            level: DiagnosticLevel::Error,
            message: format!("Failed to read SKILL.md: {e}"),
            file: path.to_path_buf(),
        })?;
        let reader = std::io::BufReader::new(file);
        let mut yaml_text = String::new();
        let mut saw_open = false;

        for line_result in reader.lines() {
            let line = line_result.map_err(|e| SkillDiagnostic {
                level: DiagnosticLevel::Error,
                message: format!("Failed to read SKILL.md: {e}"),
                file: path.to_path_buf(),
            })?;
            let trimmed = line.trim();

            if !saw_open {
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "---" {
                    saw_open = true;
                    continue;
                }
                return Err(SkillDiagnostic {
                    level: DiagnosticLevel::Error,
                    message: "SKILL.md missing YAML frontmatter (expected '---' at start)"
                        .to_string(),
                    file: path.to_path_buf(),
                });
            }

            if line.trim_end() == "---" {
                if yaml_text.ends_with('\n') {
                    yaml_text.pop();
                }
                return Ok(yaml_text);
            }
            yaml_text.push_str(&line);
            yaml_text.push('\n');
        }

        if !saw_open {
            return Err(SkillDiagnostic {
                level: DiagnosticLevel::Error,
                message: "SKILL.md missing YAML frontmatter (expected '---' at start)".to_string(),
                file: path.to_path_buf(),
            });
        }

        Err(SkillDiagnostic {
            level: DiagnosticLevel::Error,
            message: "SKILL.md frontmatter not closed (expected closing '---')".to_string(),
            file: path.to_path_buf(),
        })
    }

    /// Parse YAML frontmatter from a skill file.
    fn parse_frontmatter_yaml(
        yaml_text: &str,
        path: &Path,
    ) -> Result<(String, String), SkillDiagnostic> {
        // Try parsing with serde_yaml first
        let frontmatter: SkillFrontmatter = match serde_yaml::from_str(yaml_text) {
            Ok(fm) => fm,
            Err(_e) => {
                // Try fallback: the YAML might have issues like unquoted colons in values
                // Retry with a more lenient approach
                return Self::parse_frontmatter_fallback(yaml_text, path);
            },
        };

        let name = frontmatter.name.ok_or_else(|| SkillDiagnostic {
            level: DiagnosticLevel::Error,
            message: "SKILL.md missing required field 'name' in frontmatter".to_string(),
            file: path.to_path_buf(),
        })?;

        let description = frontmatter.description.ok_or_else(|| SkillDiagnostic {
            level: DiagnosticLevel::Error,
            message: "SKILL.md missing required field 'description' in frontmatter".to_string(),
            file: path.to_path_buf(),
        })?;

        Ok((name, description))
    }

    /// Fallback parser for malformed YAML frontmatter.
    ///
    /// Handles common issues like unquoted colons by extracting key-value pairs manually.
    fn parse_frontmatter_fallback(
        yaml_text: &str,
        path: &Path,
    ) -> Result<(String, String), SkillDiagnostic> {
        let mut name = None;
        let mut description = None;
        let mut current_key: Option<String> = None;
        let mut current_value = String::new();

        for line in yaml_text.lines() {
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }

            // Check if this is a new key (starts with non-whitespace and contains :)
            if !trimmed.starts_with(' ') && !trimmed.starts_with('\t') {
                // Save previous key-value pair
                if let Some(key) = current_key.take() {
                    let value = current_value.trim().to_string();
                    if key == "name" {
                        name = Some(value);
                    } else if key == "description" {
                        description = Some(value);
                    }
                }

                // Parse new key
                if let Some((key, value_after)) = trimmed.split_once(':') {
                    let key = key.trim().to_string();
                    let value_after = value_after.trim().to_string();
                    current_key = Some(key);
                    current_value = value_after;
                }
            } else {
                // Continuation of previous value (multiline)
                current_value.push('\n');
                current_value.push_str(trimmed);
            }
        }

        // Save the last key-value pair
        if let Some(key) = current_key {
            let value = current_value.trim().to_string();
            if key == "name" {
                name = Some(value);
            } else if key == "description" {
                description = Some(value);
            }
        }

        let name = name.ok_or_else(|| SkillDiagnostic {
            level: DiagnosticLevel::Error,
            message: "SKILL.md missing required field 'name' in frontmatter".to_string(),
            file: path.to_path_buf(),
        })?;

        let description = description.ok_or_else(|| SkillDiagnostic {
            level: DiagnosticLevel::Error,
            message: "SKILL.md missing required field 'description' in frontmatter".to_string(),
            file: path.to_path_buf(),
        })?;

        Ok((name, description))
    }

    /// Load the full body content of the skill (markdown after frontmatter).
    ///
    /// This reads from disk at activation time, not during discovery. Per-session
    /// deduplication is handled by the agent, so this method does not cache.
    pub fn load_body(&self) -> Result<String, std::io::Error> {
        let content = std::fs::read_to_string(&self.location)?;

        // Strip frontmatter
        let trimmed = content.trim_start();
        let Some(after_open) = trimmed.strip_prefix("---") else {
            return Ok(content);
        };

        if let Some((_yaml_text, body)) = after_open.split_once("\n---") {
            return Ok(body.trim_start().to_string());
        }

        Ok(content)
    }
}

// =============================================================================
// Skill Catalog
// =============================================================================

impl SkillCatalog {
    /// Create an empty skill catalog.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            skills: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Check if a path corresponds to a known skill location.
    pub fn get_skill_by_location(&self, path: &Path) -> Option<&Skill> {
        self.skills.iter().find(|s| s.location == path)
    }

    /// Filter catalog to only include specified skills.
    pub fn filter_to(&mut self, skill_names: &[String]) {
        self.skills.retain(|s| skill_names.contains(&s.name));
    }

    /// Create a disabled catalog (no skills).
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            skills: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Generate XML catalog for system prompt.
    ///
    /// Returns an empty string if no valid skills are in the catalog.
    pub fn to_prompt_xml(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut xml = String::from("<available_skills>\n");
        for skill in &self.skills {
            use std::fmt::Write;
            xml.push_str("  <skill>\n");
            _ = writeln!(xml, "    <name>{}</name>", xml_escape(&skill.name));
            _ = writeln!(
                xml,
                "    <description>{}</description>",
                xml_escape(&skill.description)
            );
            _ = writeln!(xml, "    <location>{}</location>", skill.location.display());
            xml.push_str("  </skill>\n");
        }
        xml.push_str("</available_skills>");
        xml
    }
}

/// Escape special XML characters in a string.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// =============================================================================
// Skill Discovery
// =============================================================================

/// Directories to skip during skill discovery.
const EXCLUDED_DIRS: &[&str] = &[".git", "node_modules", "target"];

/// Maximum directory depth for skill discovery.
const MAX_DEPTH: usize = 4;

/// Maximum number of directories to scan.
const MAX_DIRS: usize = 2000;

/// Discover skills from the filesystem.
///
/// Scans project-level and user-level skill directories for SKILL.md files.
/// Project skills take precedence over user skills with the same name.
///
/// # Arguments
///
/// * `working_dir` - The current working directory (for finding project-level skills)
///
/// Returns a `SkillCatalog` with discovered skills and any diagnostics.
pub fn discover_skills(working_dir: &Path) -> SkillCatalog {
    discover_skills_with_paths(working_dir, &[])
}

/// Discover skills from default locations plus configured skill roots.
///
/// Precedence is project skills, configured skills in path order, then user
/// skills. Lower-precedence skills with duplicate names are skipped.
pub fn discover_skills_with_paths(
    working_dir: &Path,
    configured_skill_dirs: &[PathBuf],
) -> SkillCatalog {
    let mut catalog = SkillCatalog::empty();
    let mut scanned_dirs = 0;

    // Scan paths: project first, configured paths next, then user.
    let project_skills_dir = working_dir.join(".agents").join("skills");
    let user_skills_dir = dirs::home_dir().map(|h| h.join(".agents").join("skills"));

    // Collect project skills first
    let mut project_skill_names = HashSet::new();
    if project_skills_dir.exists() && project_skills_dir.is_dir() {
        scan_directory(
            &project_skills_dir,
            SkillScope::Project,
            &mut catalog,
            &mut project_skill_names,
            &mut scanned_dirs,
            0,
        );
    }

    let mut configured_skill_names = HashSet::new();
    for configured_dir in configured_skill_dirs {
        if configured_dir.exists() && configured_dir.is_dir() {
            scan_directory(
                configured_dir,
                SkillScope::Configured,
                &mut catalog,
                &mut configured_skill_names,
                &mut scanned_dirs,
                0,
            );
        } else {
            catalog.diagnostics.push(SkillDiagnostic {
                level: DiagnosticLevel::Warning,
                message: "Configured skill directory does not exist or is not a directory"
                    .to_string(),
                file: configured_dir.clone(),
            });
        }
    }

    // Filter out configured skills that collide with project skills.
    catalog.skills.retain(|s| {
        if s.scope == SkillScope::Configured && project_skill_names.contains(&s.name) {
            catalog.diagnostics.push(SkillDiagnostic {
                level: DiagnosticLevel::Warning,
                message: format!(
                    "Configured skill '{}' shadowed by project skill with same name",
                    s.name
                ),
                file: s.location.clone(),
            });
            false
        } else {
            true
        }
    });

    // Then collect user skills (skip if name collision with project)
    if let Some(ref user_dir) = user_skills_dir
        && user_dir.exists()
        && user_dir.is_dir()
    {
        let mut user_skill_names = HashSet::new();
        scan_directory(
            user_dir,
            SkillScope::User,
            &mut catalog,
            &mut user_skill_names,
            &mut scanned_dirs,
            0,
        );

        // Filter out user skills that collide with project skills
        catalog.skills.retain(|s| {
            if s.scope == SkillScope::User
                && (project_skill_names.contains(&s.name)
                    || configured_skill_names.contains(&s.name))
            {
                catalog.diagnostics.push(SkillDiagnostic {
                    level: DiagnosticLevel::Warning,
                    message: format!(
                        "User skill '{}' shadowed by higher-precedence skill",
                        s.name
                    ),
                    file: s.location.clone(),
                });
                false
            } else {
                true
            }
        });
    }

    catalog
}

/// Parse a platform-separated skill path string and expand `~` to the home directory.
pub fn parse_skill_path_list(path_list: &str) -> Vec<PathBuf> {
    std::env::split_paths(path_list)
        .filter(|p| !p.as_os_str().is_empty())
        .map(expand_home)
        .collect()
}

fn expand_home(path: PathBuf) -> PathBuf {
    let Some(path_str) = path.to_str() else {
        return path;
    };

    if path_str == "~" {
        if let Some(home_dir) = dirs::home_dir() {
            return home_dir;
        }
        return path;
    }

    if let Some(rest) = path_str
        .strip_prefix("~/")
        .or_else(|| path_str.strip_prefix("~\\"))
        && let Some(home_dir) = dirs::home_dir()
    {
        return home_dir.join(rest);
    }

    path
}

/// Recursively scan a directory for SKILL.md files.
fn scan_directory(
    dir: &Path,
    scope: SkillScope,
    catalog: &mut SkillCatalog,
    names_seen: &mut HashSet<String>,
    scanned_dirs: &mut usize,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    if *scanned_dirs >= MAX_DIRS {
        catalog.diagnostics.push(SkillDiagnostic {
            level: DiagnosticLevel::Warning,
            message: format!("Skill discovery stopped after scanning {MAX_DIRS} directories"),
            file: dir.to_path_buf(),
        });
        return;
    }
    *scanned_dirs += 1;

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            catalog.diagnostics.push(SkillDiagnostic {
                level: DiagnosticLevel::Warning,
                message: format!("Failed to read directory: {e}"),
                file: dir.to_path_buf(),
            });
            return;
        },
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Skip excluded directories
        if EXCLUDED_DIRS.contains(&name) {
            continue;
        }

        // Check if this directory contains a SKILL.md
        let skill_md = path.join("SKILL.md");
        if skill_md.exists() && skill_md.is_file() {
            match Skill::parse(&skill_md, scope) {
                Ok(skill) => {
                    if names_seen.contains(&skill.name) {
                        catalog.diagnostics.push(SkillDiagnostic {
                            level: DiagnosticLevel::Warning,
                            message: format!(
                                "Duplicate skill name '{}' within same scope, using first found",
                                skill.name
                            ),
                            file: skill.location.clone(),
                        });
                    } else {
                        names_seen.insert(skill.name.clone());
                        catalog.skills.push(skill);
                    }
                },
                Err(diagnostic) => {
                    catalog.diagnostics.push(diagnostic);
                },
            }
        }

        // Recurse into subdirectories
        scan_directory(&path, scope, catalog, names_seen, scanned_dirs, depth + 1);
    }
}

// =============================================================================
// Skill Configuration
// =============================================================================

/// Resolved skill configuration from CLI and settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillConfig {
    /// Load all discovered skills
    All,
    /// Don't load any skills
    Disabled,
    /// Load only these named skills
    Only(Vec<String>),
}

impl SkillConfig {
    /// Apply this configuration to a skill catalog.
    ///
    /// Returns a new catalog with skills filtered according to the configuration.
    pub fn apply(&self, mut catalog: SkillCatalog) -> SkillCatalog {
        match *self {
            Self::All => catalog,
            Self::Disabled => SkillCatalog::disabled(),
            Self::Only(ref names) => {
                catalog.filter_to(names);
                catalog
            },
        }
    }
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
