//! The start note's crowded-place count and `serve why` count one thing: the
//! facts that really compete in the block of a call. A fact bound to a PATH
//! that the command names is in that block too (`ServeInput::add_command`),
//! so both must count it (field 2026-10: the start note listed 5 facts at a
//! place, `serve why` 6).

use model::item::{Binding, Item, Kind, TargetKind};
use serve::input::ServeInput;
use thor_core::event_store::EventStore;

fn note(id: &str, words: &str, binding: Binding) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("{id}: {words}"),
        bindings: vec![binding],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("it stops being true".to_string()),
        check: None,
    }
}

#[test]
fn a_path_bound_fact_counts_in_the_crowded_place_and_in_serve_why_alike() {
    let command = "git add docs/board.md";
    let mut store = EventStore::in_memory().unwrap();
    for i in 0..3 {
        let binding = Binding::Target { kind: TargetKind::Command, value: command.to_string() };
        model::store::declare(&mut store, "s", "s", "owner", &note(&format!("on-command-{i}"), ["ribbon cable seating", "bed leveling probe", "firmware flash order"][i], binding)).unwrap();
    }
    for i in 0..3 {
        let binding = Binding::Target { kind: TargetKind::Path, value: "docs/board.md".to_string() };
        model::store::declare(&mut store, "s", "s", "owner", &note(&format!("on-path-{i}"), ["thermistor wiring colours", "stepper driver current", "enclosure fan curve"][i], binding)).unwrap();
    }

    let mut input = ServeInput::default();
    input.add_command(command);
    let served = serve::serve(&store, &input);
    let why = served.all.len();

    let places = model::store::outranked_places(&store, "p");
    let place = places.iter().find(|p| p.kind == TargetKind::Command).expect("the command place is crowded");
    assert_eq!(why, 6, "serve why counts the path-bound facts too");
    assert_eq!(place.occupants.len(), why, "the start note's place holds the same facts: {:?}", place.occupants);
}
