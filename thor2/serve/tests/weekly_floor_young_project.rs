//! The weekly floor counts from the project's first activity in this memory: a project that
//! began two days ago has not gone a week without an evaluation, whatever it did since; one that
//! began nine days ago has.

use model::item::{Binding, Item, Kind, TargetKind};
use serve::usefulness::{project_is_young, weekly_floor_due};
use thor_core::event_store::{EventKind, EventStore};

const DAY: i64 = 86_400;
const SESSION: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";

fn item(id: &str, kind: Kind, bindings: Vec<Binding>, tags: Vec<String>) -> Item {
    Item {
        id: id.to_string(),
        kind,
        text: format!("Fixture record {id} about fxp, kept for the weekly floor."),
        bindings,
        severity: None,
        project: Some("fxp".to_string()),
        tags,
        expires: None,
        key: None,
        falsifier: Some("the fixture is wrong".to_string()),
        check: None,
    }
}

#[test]
fn the_weekly_floor_does_not_ask_a_project_younger_than_a_week() {
    let now = serve::time::now_unix();
    let at = |days: i64| serve::time::iso8601_from_unix(now - days * DAY);
    let actor = serve::usefulness::ASSISTANT_DELIVERY_ACTOR;
    for (began_days_ago, due) in [(2, false), (9, true)] {
        let mut store = EventStore::in_memory().unwrap();
        let rule = item("fx-rule", Kind::Rule, vec![Binding::Target { kind: TargetKind::Command, value: "fx-cmd".to_string() }], vec![]);
        model::store::declare(&mut store, "fixture", "fixture", "fixture", &rule).unwrap();
        serve::deliver::record_delivery(&mut store, "fixture", "fixture", actor, &at(began_days_ago), &["seed".to_string()]);
        // The installer's record of the project, written the way the installer writes it.
        let opened = item(&model::store::project_opened_id("fxp"), Kind::Report, vec![], vec!["project-record".to_string()]);
        store
            .append_event("install", &opened.id, "installer", EventKind::FactCreated, &opened.id, None, &serde_json::to_string(&opened).unwrap())
            .unwrap();
        serve::deliver::record_delivery(&mut store, SESSION, SESSION, actor, &at(1), &["fx-rule".to_string()]);
        assert_eq!(project_is_young(&store, "fxp", now), began_days_ago < 7);
        assert_eq!(weekly_floor_due(&store, "fxp", now), due, "began {began_days_ago} days ago");
    }
}
