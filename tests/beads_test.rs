//! Tests for the Beads integration layer.
//!
//! These tests validate JSON parsing against the shapes `bd` actually returns.

use serde_json;

/// The Issue struct we're testing against.
#[derive(Debug, Clone, serde::Deserialize)]
struct Issue {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub description: Option<String>,
    pub priority: u8,
    pub issue_type: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[test]
fn parse_bd_ready_output() {
    let json = r#"[
        {
            "id": "bd-a1b2",
            "title": "replay should be able to read an issue and produce a patch",
            "status": "open",
            "priority": 1,
            "issue_type": "feature"
        }
    ]"#;

    let issues: Vec<Issue> = serde_json::from_str(json).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].id, "bd-a1b2");
    assert_eq!(issues[0].status, "open");
    assert_eq!(issues[0].priority, 1);
    assert!(issues[0].description.is_none());
}

#[test]
fn parse_bd_show_output() {
    let json = r#"{
        "id": "bd-a1b2",
        "title": "replay should be able to read an issue and produce a patch",
        "status": "in_progress",
        "description": "The agent should read a beads issue and use SmartRead/SmartWrite to solve it.",
        "priority": 1,
        "issue_type": "feature",
        "assignee": "replay",
        "labels": ["bootstrap"]
    }"#;

    let issue: Issue = serde_json::from_str(json).unwrap();
    assert_eq!(issue.id, "bd-a1b2");
    assert_eq!(issue.status, "in_progress");
    assert_eq!(
        issue.description.as_deref(),
        Some("The agent should read a beads issue and use SmartRead/SmartWrite to solve it.")
    );
    assert_eq!(issue.assignee.as_deref(), Some("replay"));
    assert_eq!(issue.labels, vec!["bootstrap"]);
}

#[test]
fn parse_empty_ready_list() {
    let json = "[]";
    let issues: Vec<Issue> = serde_json::from_str(json).unwrap();
    assert!(issues.is_empty());
}

#[test]
fn parse_bd_create_output() {
    let json = r#"{
        "id": "bd-c3d4",
        "title": "Fix: replay should be able to read an issue",
        "status": "open",
        "priority": 2,
        "issue_type": "task"
    }"#;

    let issue: Issue = serde_json::from_str(json).unwrap();
    assert_eq!(issue.id, "bd-c3d4");
    assert_eq!(issue.issue_type, "task");
}
