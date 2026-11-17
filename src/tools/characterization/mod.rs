//! Characterization tests: pin the public JSON of the four image tools,
//! including key order, notes and file names.
//! Expected values are literals; if one has to change, behavior changed.

mod batch;
mod edit;
mod generate;
mod multi_reference;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::Value;

use super::{
    ToolEngine,
    test_support::{ScriptedProvider, models_and_formats, scripted, tool_engine},
};

/// Size note for a 32x24 answer to a 1024x1024 request that asked for no GIF.
const SIZE_NOTE_32X24_FOR_1024: &str = "⚠ 实际 32×24 (0.00MP) ≠ 请求 1024×1024 (1.05MP)；米醋后端可能重映射自定义尺寸。需要精确像素时请使用 gpt-image-2-openai，并核对 saved.actual_size。";
/// The same note for a 2048x1152 request.
const SIZE_NOTE_32X24_FOR_2048X1152: &str = "⚠ 实际 32×24 (0.00MP) ≠ 请求 2048×1152 (2.36MP)；米醋后端可能重映射自定义尺寸。需要精确像素时请使用 gpt-image-2-openai，并核对 saved.actual_size。";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    engine: ToolEngine,
    provider: Arc<ScriptedProvider>,
}

impl Fixture {
    fn new(provider: ScriptedProvider) -> Self {
        Self::with_api_key(provider, "sk-test")
    }

    /// Outputs go to `<root>/out`. The input root is `<root>` itself, so a
    /// session can read its previous output back as the next input.
    fn with_api_key(provider: ScriptedProvider, api_key: &str) -> Self {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let root = temp
            .path()
            .canonicalize()
            .unwrap_or_else(|error| panic!("{error}"));
        std::fs::create_dir_all(root.join("input")).unwrap_or_else(|error| panic!("{error}"));
        let out = root.join("out").to_string_lossy().into_owned();
        let environment = BTreeMap::from([
            ("MICU_SAVE_DIR_ROOT".into(), out.clone()),
            ("MICU_SAVE_DIR".into(), out),
            (
                "MICU_INPUT_ROOT".into(),
                root.to_string_lossy().into_owned(),
            ),
            ("MICU_API_KEY".into(), api_key.to_owned()),
            ("MICU_BASEURL".into(), "http://127.0.0.1:9".into()),
        ]);
        let provider = Arc::new(provider);
        let engine = tool_engine(&root, environment, provider.clone());
        Self {
            _temp: temp,
            root,
            engine,
            provider,
        }
    }

    /// Writes `<root>/input/<name>` and returns its path as a caller would pass it.
    fn input(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.root.join("input").join(name);
        std::fs::write(&path, bytes).unwrap_or_else(|error| panic!("{error}"));
        path.to_string_lossy().into_owned()
    }

    /// Path of a not-yet-written revision record of session `id`.
    fn revision_record(&self, id: &str, revision: u32) -> PathBuf {
        self.root
            .join("out")
            .join(".micu-sessions")
            .join(id)
            .join(format!("revision-{revision}.json"))
    }

    /// Asserts the serialized result, so key order counts. The temp root, session
    /// IDs and default-basename timestamps are masked first.
    #[track_caller]
    fn assert_public_json(&self, actual: &Value, expected: Value) {
        let mut actual = actual.clone();
        mask_volatile(&mut actual, &self.root);
        let actual_text = actual.to_string();
        let expected_text = expected.to_string();
        assert!(
            actual_text == expected_text,
            "public JSON differs\n--- actual ---\n{actual:#}\n--- expected ---\n{expected:#}"
        );
    }
}

fn params<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap_or_else(|error| panic!("{error}"))
}

fn session_id(result: &Value) -> String {
    result["session"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no session id in {result}"))
        .to_owned()
}

fn mask_volatile(value: &mut Value, root: &Path) {
    match value {
        Value::String(text) => {
            let masked = text.replace(root.to_string_lossy().as_ref(), "<TMP>");
            *text = mask_digit_runs(&mask_session_ids(&masked));
        }
        Value::Array(items) => items.iter_mut().for_each(|item| mask_volatile(item, root)),
        Value::Object(object) => object
            .values_mut()
            .for_each(|item| mask_volatile(item, root)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// `edit_` followed by exactly 32 lowercase hex digits is a session ID.
fn mask_session_ids(text: &str) -> String {
    const PREFIX: &str = "edit_";
    let mut masked = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(PREFIX) {
        let after = &rest[start + PREFIX.len()..];
        let hex_digits = after
            .bytes()
            .take_while(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            .count();
        masked.push_str(&rest[..start + PREFIX.len()]);
        if hex_digits == 32 {
            masked.push_str("<SESSION>");
            rest = &after[hex_digits..];
        } else {
            rest = after;
        }
    }
    masked.push_str(rest);
    masked
}

/// Default basenames embed nanoseconds since the epoch: a run of 16 or more digits.
fn mask_digit_runs(text: &str) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut run = String::new();
    for character in text.chars() {
        if character.is_ascii_digit() {
            run.push(character);
            continue;
        }
        flush_digit_run(&mut masked, &mut run);
        masked.push(character);
    }
    flush_digit_run(&mut masked, &mut run);
    masked
}

fn flush_digit_run(masked: &mut String, run: &mut String) {
    if run.len() >= 16 {
        masked.push_str("<NANOS>");
    } else {
        masked.push_str(run);
    }
    run.clear();
}
