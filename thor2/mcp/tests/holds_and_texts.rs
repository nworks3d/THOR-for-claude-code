//! Holds, refusals and the texts around them, end to end through the real
//! compiled tool server: a folder for a subject is never "crowded", and the
//! other plain-text answers a first-time user reads.

mod common;

use common::*;
use serde_json::json;

fn orientation(id: &str, project: &str) -> serde_json::Value {
    json!({
        "id": id,
        "kind": "orientation",
        "text": format!("Fact {id} about the garden shed."),
        "project": project,
        "targets": [{"kind": "project", "value": project}],
        "falsifier": "A later note says the shed was moved.",
    })
}

/// THE CONTROL: the same seven notes in a git project are warned about, so the
/// test above can fail.
#[test]
fn a_git_project_is_still_warned_about_a_crowded_project_binding() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["shop"]);
    let root = checkout(dir.path(), "shop", &[("a.txt", "x")], true);
    register(&db, "shop", &root);
    let mut mcp = Mcp::start(&db, &root);
    let warned = (0..7).any(|n| mcp.call("remember", orientation(&format!("shed-{n}"), "shop")).contains("never be shown"));
    assert!(warned, "the control must show the warning that the subject folder does not get");
}

/// In a folder for a subject (own marker, no git) a note bound to its own
/// project is found by matching the person's prompt, not by place, so the
/// seventh of them is not "crowded": no warning in the reply.
#[test]
fn a_folder_for_a_subject_never_warns_that_a_note_may_never_be_shown() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["garden"]);
    let topic = dir.path().join("garden");
    std::fs::create_dir_all(&topic).unwrap();
    std::fs::write(topic.join(".thor-project"), "garden\n").unwrap();
    register(&db, "garden", &topic);
    let mut mcp = Mcp::start(&db, &topic);
    for n in 0..7 {
        let reply = mcp.call("remember", orientation(&format!("shed-{n}"), "garden"));
        assert!(reply.starts_with("stored"), "{reply}");
        assert!(!reply.contains("never be shown"), "a subject folder is not crowded: {reply}");
    }
}
