//! main/Pagesへ出す既知互換性台帳のリリースゲート。

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

#[test]
fn every_registered_compatibility_entry_passes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes = fs::read(root.join("compatibility/catalog.json")).expect("read catalog.json");
    let catalog: Value = serde_json::from_slice(&bytes).expect("catalog must be valid JSON");
    let entries = catalog["entries"]
        .as_array()
        .expect("entries must be an array");
    assert!(
        !entries.is_empty(),
        "compatibility catalog must not be empty"
    );

    let mut ids = BTreeSet::new();
    for entry in entries {
        let id = entry["id"].as_str().expect("entry id must be a string");
        assert!(ids.insert(id), "duplicate compatibility id: {id}");
        assert_eq!(
            entry["status"].as_str(),
            Some("pass"),
            "registered compatibility entry {id} is not passing"
        );
        assert!(
            entry["license"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "{id} must record redistribution license"
        );
        let models = entry["models"].as_array().expect("models must be an array");
        assert!(!models.is_empty(), "{id} must register at least one model");
        let unique: BTreeSet<_> = models.iter().filter_map(Value::as_str).collect();
        assert_eq!(
            unique.len(),
            models.len(),
            "{id} has duplicate/invalid models"
        );
    }

    let diagnostic = entries
        .iter()
        .find(|entry| entry["id"] == "builtin-diagnostic")
        .expect("built-in diagnostic must remain registered");
    let models: BTreeSet<_> = diagnostic["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(models, BTreeSet::from(["x68000", "x68030", "xvi"]));
}
