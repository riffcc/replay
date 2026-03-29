//! Skill discovery and `$skill-name` prompt reference parsing.

use std::collections::HashSet;
use std::path::Path;

use llm_code_sdk::SkillRegistry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSkill {
    pub name: String,
    pub description: String,
}

impl AsRef<str> for DiscoveredSkill {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillReference {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

fn is_skill_start_char(c: char) -> bool {
    c.is_ascii_alphabetic()
}

fn is_skill_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

fn has_wordish_prefix(input: &str, idx: usize) -> bool {
    input[..idx]
        .chars()
        .next_back()
        .map(|c| c.is_ascii_alphanumeric() || c == '-')
        .unwrap_or(false)
}

pub fn discover_registry(project_root: &Path) -> SkillRegistry {
    let mut registry = SkillRegistry::new();

    if let Some(home) = dirs::home_dir() {
        registry.discover(&home.join(".replay").join("skills"));
    }
    registry.discover(&project_root.join(".replay").join("skills"));

    registry
}

pub fn discover_skills(project_root: &Path) -> Vec<DiscoveredSkill> {
    let registry = discover_registry(project_root);
    let mut skills: Vec<DiscoveredSkill> = registry
        .list()
        .into_iter()
        .filter_map(|name| {
            registry.get(name).map(|skill| DiscoveredSkill {
                name: skill.meta.name.clone(),
                description: skill.meta.description.clone(),
            })
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn discover_skill_names(project_root: &Path) -> Vec<String> {
    discover_skills(project_root)
        .into_iter()
        .map(|skill| skill.name)
        .collect()
}

pub fn find_skill_references<T: AsRef<str>>(input: &str, available_skills: &[T]) -> Vec<SkillReference> {
    if input.is_empty() || available_skills.is_empty() {
        return Vec::new();
    }

    let known: HashSet<&str> = available_skills.iter().map(|skill| skill.as_ref()).collect();
    let mut refs = Vec::new();
    let mut i = 0;

    while i < input.len() {
        let Some(ch) = input[i..].chars().next() else {
            break;
        };

        if ch != '$' {
            i += ch.len_utf8();
            continue;
        }

        if has_wordish_prefix(input, i) {
            i += ch.len_utf8();
            continue;
        }

        let name_start = i + ch.len_utf8();
        let mut end = name_start;
        let mut chars = input[name_start..].char_indices();

        match chars.next() {
            Some((_, first)) if is_skill_start_char(first) => {
                end += first.len_utf8();
            }
            _ => {
                i += ch.len_utf8();
                continue;
            }
        }

        for (offset, c) in chars {
            if !is_skill_char(c) {
                break;
            }
            end = name_start + offset + c.len_utf8();
        }

        let name = &input[name_start..end];
        if known.contains(name) {
            refs.push(SkillReference {
                name: name.to_string(),
                start: i,
                end,
            });
            i = end;
        } else {
            i += ch.len_utf8();
        }
    }

    refs
}

pub fn extract_skill_references<T: AsRef<str>>(input: &str, available_skills: &[T]) -> Vec<String> {
    let mut names = Vec::new();
    for reference in find_skill_references(input, available_skills) {
        if !names.iter().any(|name| name == &reference.name) {
            names.push(reference.name);
        }
    }
    names
}

pub fn strip_skill_references<T: AsRef<str>>(input: &str, available_skills: &[T]) -> String {
    let references = find_skill_references(input, available_skills);
    if references.is_empty() {
        return input.to_string();
    }

    let mut stripped = String::with_capacity(input.len());
    let mut cursor = 0;

    for reference in references {
        if reference.start > cursor {
            stripped.push_str(&input[cursor..reference.start]);
        }
        cursor = reference.end;
    }

    if cursor < input.len() {
        stripped.push_str(&input[cursor..]);
    }

    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skills() -> Vec<String> {
        vec!["lief-persona".to_string(), "riff-commit".to_string()]
    }

    #[test]
    fn finds_valid_skill_references() {
        let refs = find_skill_references("please use $lief-persona and $riff-commit", &skills());
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name, "lief-persona");
        assert_eq!(refs[1].name, "riff-commit");
    }

    #[test]
    fn ignores_unknown_or_embedded_skill_references() {
        let refs = find_skill_references("foo$lief-persona $unknown", &skills());
        assert!(refs.is_empty());
    }

    #[test]
    fn strips_recognized_skill_references_from_prompt() {
        let stripped = strip_skill_references("please use $lief-persona for this", &skills());
        assert_eq!(stripped, "please use for this");
    }
}
