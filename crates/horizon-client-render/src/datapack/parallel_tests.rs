use super::*;
use std::io::{Cursor, Write};

#[test]
fn zip_budget_keeps_the_first_resource_in_file_order() {
    // The first JSON takes longer to inflate than the later files, but must
    // consume the budget first. A small budget exercises the real ZIP gate
    // without allocating gigabytes.
    let first = format!("{{}}{}", " ".repeat(128 * 1024));
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("a", first.as_bytes()),
        ("b", b"{}"),
        ("c", b"{}"),
        ("d", b"{}"),
    ] {
        writer
            .start_file(
                format!("data/t/predicate/{name}.json"),
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    let data = writer.finish().unwrap().into_inner();
    let pack = DataPack {
        id: "file/budget.zip".into(),
        source: Source::Zip(Box::new(
            Zip::from_bytes(data)
                .unwrap()
                .with_read_budget(first.len() as u64),
        )),
    };
    let store = DataStore::load(&[&pack]);
    assert_eq!(
        store
            .predicates
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["t:a"]
    );
    assert_eq!(store.predicates["t:a"], serde_json::json!({}));
}

#[test]
fn loaded_slot_source_tags_replace_then_append_in_pack_order() {
    let root = temp_folder().unwrap();
    let mut packs = Vec::new();
    for (name, tag) in [
        ("lower", r#"{"values":["t:a","t:b"]}"#),
        ("replace", r#"{"replace":true,"values":["t:b"]}"#),
        ("append", r#"{"values":["t:a"]}"#),
    ] {
        let dir = root.join(name);
        for (file, json) in [
            ("tags/slot_source/all", tag),
            (
                "slot_source/a",
                r#"{"type":"slot_range","slots":"container.0"}"#,
            ),
            (
                "slot_source/b",
                r#"{"type":"slot_range","slots":"container.1"}"#,
            ),
            ("predicate/p", "{}"),
        ] {
            let path = dir.join(format!("data/t/{file}.json"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, json).unwrap();
        }
        packs.push(DataPack {
            id: format!("file/{name}"),
            source: Source::Dir(dir),
        });
    }
    let store = DataStore::load(&packs.iter().collect::<Vec<_>>());
    assert_eq!(store.slot_source_tag("t:all").unwrap(), ["t:b", "t:a"]);
    assert!(!store.files.contains_key("tags/t:slot_source/all"));
    std::fs::remove_dir_all(root).unwrap();
}
