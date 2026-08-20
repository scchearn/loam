use std::fs;

use loam::json::Value;

fn fixture() -> Value {
    let source = include_str!("fixtures/view/portfolio-snapshot-cases.json");
    loam::json::parse(source).expect("portfolio fixture must be valid JSON")
}

#[test]
fn portfolio_fixture_covers_all_supported_path_families() {
    let root = fixture();
    assert_eq!(
        root.get("schema").and_then(Value::as_str),
        Some("loam.view.portfolio-fixtures/v1")
    );
    assert_eq!(
        root.get("body_fields_forbidden")
            .and_then(|value| match value {
                Value::Bool(value) => Some(*value),
                _ => None,
            }),
        Some(true)
    );

    let cases = root
        .get("cases")
        .and_then(Value::as_array)
        .expect("fixture cases");
    assert_eq!(cases.len(), 3);
    assert_eq!(
        cases
            .iter()
            .filter_map(|case| case.get("platform").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        vec!["linux", "macos", "windows"]
    );
}

#[test]
fn portfolio_fixture_enforces_path_state_and_disclosure_invariants() {
    let root = fixture();
    let cases = root
        .get("cases")
        .and_then(Value::as_array)
        .expect("fixture cases");
    let allowed_states = ["enrolled", "discovered", "missing"];
    let allowed_fields = [
        "path_key",
        "path_display",
        "state",
        "counts",
        "items",
        "next_action",
    ];

    let mut states = Vec::new();
    for case in cases {
        let snapshots = case
            .get("snapshots")
            .and_then(Value::as_array)
            .expect("case snapshots");
        for snapshot in snapshots {
            let object = match snapshot {
                Value::Object(entries) => entries,
                _ => panic!("snapshot must be an object"),
            };
            for (field, _) in object {
                assert!(
                    allowed_fields.contains(&field.as_str()),
                    "unexpected snapshot field {field}"
                );
                assert_ne!(field, "body");
            }
            let path_key = snapshot
                .get("path_key")
                .and_then(Value::as_str)
                .expect("path_key");
            assert!(!path_key.is_empty());
            assert!(
                path_key.contains('/'),
                "path_key must use normalized separators: {path_key}"
            );
            let state = snapshot
                .get("state")
                .and_then(Value::as_str)
                .expect("state");
            assert!(allowed_states.contains(&state), "unknown state {state}");
            states.push(state);

            if state == "missing" {
                assert!(snapshot.get("counts").is_some_and(Value::is_null));
            } else {
                assert!(snapshot
                    .get("counts")
                    .is_some_and(|value| matches!(value, Value::Object(_))));
            }
        }
    }

    assert!(states.contains(&"enrolled"));
    assert!(states.contains(&"discovered"));
    assert!(states.contains(&"missing"));
}

#[test]
fn fixture_is_read_from_the_repository_and_has_no_machine_specific_output() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("view")
        .join("portfolio-snapshot-cases.json");
    assert!(path.is_file());
    let source = fs::read_to_string(path).expect("fixture should be readable");
    assert!(!source.contains("\r\n"));
    assert!(!source.contains("C:\\Users\\"));
}
