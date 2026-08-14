//! Pi oracle for the settings layer (spec §6).
//!
//! Replays every record of `tests/fixtures/pi/cli/settings.merge.cases.jsonl` — captured by
//! executing real Pi — against [`pirust_coding_agent::settings`]. Nothing here asserts a
//! self-authored expectation: each record carries Pi's literal `result` / `resultKeys` /
//! `errors` / `diagnostics` and those are what is compared.
//!
//! Three functions are covered, and the record count of each is asserted so a shrunken
//! fixture fails loudly rather than silently passing:
//!
//! | `fn` | records |
//! |---|---|
//! | `deepMergeSettings` | 30 |
//! | `migrateSettings` | 20 |
//! | `SettingsManager.fromStorage` | 12 |
//!
//! Failures are **collected**, not `panic!`ed one at a time, so one run reports every
//! divergence with the case name and expected-vs-actual.
//!
//! # `{"$undefined": true}`
//!
//! The fixture encodes JS `undefined` — a key that is present with no value, which JSON
//! cannot express — as that sentinel object, and so does
//! [`pirust_coding_agent::settings`] (see its module docs). Fixture values are therefore fed
//! in and compared **verbatim**, with no translation layer that could paper over a
//! difference.
//!
//! # The one thing that is not compared literally
//!
//! Seven records are marked `v8Dependent: true`. Three of them are
//! `TypeError: Cannot use 'in' operator to search for 'queueMode' in <value>`, whose wording
//! is deterministic — those **are** asserted on exact text. The other four are `SyntaxError`s
//! from `JSON.parse`, e.g.
//!
//! ```text
//! Expected property name or '}' in JSON at position 2 (line 1 column 3)
//! ```
//!
//! That is V8's phrasing of the defect. `serde_json` describes the same defect in its own
//! words (`key must be a string at line 1 column 3`) and no amount of care recovers V8's
//! string without shipping a second JSON parser, so for those four the assertions cover the
//! *structure* Pi produces — which scope failed, how many errors were queued and in what
//! order, the error class (`SyntaxError`), the diagnostic type and prefix, that the scope
//! fell back to `{}`, that the other scope still loaded, and that `drainErrors` is
//! idempotent — while the message text is recorded as divergent. The number of exempted
//! errors is itself asserted (five queued errors across those four records —
//! `malformed:both-scopes-unparseable` queues two), so the exemption cannot quietly widen.

use std::path::PathBuf;

use pirust_coding_agent::settings::{
    deep_merge_settings, json_stringify, migrate_settings, InMemorySettingsStorage,
    SettingsManager, SettingsManagerCreateOptions, SettingsMap, SettingsScope,
};
use serde_json::Value;

/// Workspace-root fixtures dir (`CARGO_MANIFEST_DIR` = `crates/pirust-coding-agent`).
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/cli/settings.merge.cases.jsonl")
}

fn load_records() -> Vec<Value> {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("line {}: {error}\n  {line}", index + 1))
        })
        .collect()
}

/// Collected divergences, so one run reports all of them.
#[derive(Default)]
struct Failures(Vec<String>);

impl Failures {
    fn eq<T: std::fmt::Debug + PartialEq>(&mut self, case: &str, what: &str, want: T, got: T) {
        if want != got {
            self.0.push(format!(
                "[{case}] {what}\n      expected: {want:?}\n      actual:   {got:?}"
            ));
        }
    }

    fn assert_clean(self, total: usize) {
        if !self.0.is_empty() {
            panic!(
                "{} of {total} fixture record(s) diverged from Pi:\n  {}",
                self.0.len(),
                self.0.join("\n  ")
            );
        }
    }
}

fn field<'a>(record: &'a Value, key: &str) -> &'a Value {
    record
        .get(key)
        .unwrap_or_else(|| panic!("fixture record missing `{key}`: {record}"))
}

fn name_of(record: &Value) -> String {
    field(record, "name")
        .as_str()
        .expect("`name` is a string")
        .to_string()
}

/// The ordered key list the fixture stores as `resultKeys`.
fn keys_of(map: &SettingsMap) -> Vec<String> {
    map.keys().cloned().collect()
}

fn expected_keys(record: &Value) -> Vec<String> {
    field(record, "resultKeys")
        .as_array()
        .expect("`resultKeys` is an array")
        .iter()
        .map(|key| key.as_str().expect("a key is a string").to_string())
        .collect()
}

#[test]
fn settings_fixture_has_every_record() {
    let records = load_records();
    assert_eq!(records.len(), 62, "fixture should have all 62 records");

    let count = |name: &str| {
        records
            .iter()
            .filter(|record| field(record, "fn") == name)
            .count()
    };
    assert_eq!(count("deepMergeSettings"), 30, "deepMergeSettings records");
    assert_eq!(count("migrateSettings"), 20, "migrateSettings records");
    assert_eq!(
        count("SettingsManager.fromStorage"),
        12,
        "SettingsManager.fromStorage records"
    );
    assert_eq!(
        count("deepMergeSettings")
            + count("migrateSettings")
            + count("SettingsManager.fromStorage"),
        records.len(),
        "every record should be dispatched by one of the three arms"
    );

    // The two rows that distinguish a shallow merge from a deep one, and a replacing array
    // from a concatenating one, must be present by name.
    for required in [
        "CRITICAL-two-levels-deep-is-NOT-merged",
        "CRITICAL-array-in-both-is-REPLACED",
    ] {
        assert!(
            records.iter().any(|record| name_of(record) == required),
            "fixture should still contain `{required}`"
        );
    }
}

#[test]
fn deep_merge_settings_matches_pi() {
    let records = load_records();
    let mut failures = Failures::default();
    let mut replayed = 0;

    for record in &records {
        if field(record, "fn") != "deepMergeSettings" {
            continue;
        }
        replayed += 1;
        let case = name_of(record);
        let global = field(record, "global");
        let project = field(record, "project");

        let merged = deep_merge_settings(global, project);

        failures.eq(
            &case,
            "result",
            field(record, "result").clone(),
            Value::Object(merged.clone()),
        );
        failures.eq(&case, "resultKeys", expected_keys(record), keys_of(&merged));

        // The merge must not mutate either input (Pi builds a fresh `{...base}`).
        failures.eq(
            &case,
            "global is not mutated",
            field(record, "global").clone(),
            global.clone(),
        );
        failures.eq(
            &case,
            "project is not mutated",
            field(record, "project").clone(),
            project.clone(),
        );
    }

    assert_eq!(
        replayed, 30,
        "should replay all 30 deepMergeSettings records"
    );
    failures.assert_clean(replayed);
}

#[test]
fn migrate_settings_matches_pi() {
    let records = load_records();
    let mut failures = Failures::default();
    let mut replayed = 0;

    for record in &records {
        if field(record, "fn") != "migrateSettings" {
            continue;
        }
        replayed += 1;
        let case = name_of(record);

        // Pi mutates its argument in place and returns the same object; every record says
        // `mutatesInputInPlace: true`, so the assertion is made on the mutated input itself.
        assert_eq!(
            field(record, "mutatesInputInPlace"),
            &Value::Bool(true),
            "[{case}] fixture no longer claims in-place mutation"
        );
        let mut settings = field(record, "input").clone();
        let outcome = migrate_settings(&mut settings);

        failures.eq(
            &case,
            "ok",
            field(record, "ok").as_bool().expect("`ok` is a bool"),
            outcome.is_ok(),
        );
        failures.eq(
            &case,
            "result",
            field(record, "result").clone(),
            settings.clone(),
        );
        failures.eq(
            &case,
            "resultKeys",
            expected_keys(record),
            settings
                .as_object()
                .map(keys_of)
                .unwrap_or_else(|| panic!("[{case}] migrated value is not an object: {settings}")),
        );
    }

    assert_eq!(replayed, 20, "should replay all 20 migrateSettings records");
    failures.assert_clean(replayed);
}

#[test]
fn from_storage_matches_pi() {
    let records = load_records();
    let mut failures = Failures::default();
    let mut replayed = 0;
    let mut v8_syntax_exemptions = 0;

    for record in &records {
        if field(record, "fn") != "SettingsManager.fromStorage" {
            continue;
        }
        replayed += 1;
        let case = name_of(record);

        let raw = |key: &str| match field(record, key) {
            Value::String(text) => Some(text.clone()),
            Value::Null => None,
            other => panic!("[{case}] `{key}` should be a string or null, got {other}"),
        };
        let options = SettingsManagerCreateOptions {
            project_trusted: field(record, "options")
                .get("projectTrusted")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        };

        // `thrown: false` on every record: `fromStorage` captures both load failures instead
        // of propagating them, which in Rust is the absence of an `Err`/panic below.
        failures.eq(
            &case,
            "thrown",
            field(record, "thrown")
                .as_bool()
                .expect("`thrown` is a bool"),
            false,
        );

        let storage = InMemorySettingsStorage::with_contents(raw("globalRaw"), raw("projectRaw"));
        let mut manager = SettingsManager::from_storage(std::sync::Arc::new(storage), options);

        failures.eq(
            &case,
            "globalSettings",
            field(record, "globalSettings").clone(),
            manager.get_global_settings(),
        );
        failures.eq(
            &case,
            "projectSettings",
            field(record, "projectSettings").clone(),
            manager.get_project_settings(),
        );
        failures.eq(
            &case,
            "mergedSettings",
            field(record, "mergedSettings").clone(),
            Value::Object(manager.merged_settings().clone()),
        );

        // --- the drainable error queue -------------------------------------------------
        let expected_errors = field(record, "errors")
            .as_array()
            .expect("`errors` is an array");
        let expected_diagnostics = field(record, "diagnostics")
            .as_array()
            .expect("`diagnostics` is an array");
        let drained = manager.drain_errors();

        failures.eq(&case, "errors.length", expected_errors.len(), drained.len());
        failures.eq(
            &case,
            "diagnostics.length",
            expected_diagnostics.len(),
            drained.len(),
        );

        for (index, expected) in expected_errors.iter().enumerate() {
            let Some(actual) = drained.get(index) else {
                continue;
            };
            let label = format!("errors[{index}]");
            let expected_name = field(expected, "name").as_str().expect("`name`");
            let expected_scope = field(expected, "scope").as_str().expect("`scope`");
            let expected_message = field(expected, "message").as_str().expect("`message`");

            failures.eq(
                &case,
                &format!("{label}.scope"),
                expected_scope,
                actual.scope.as_str(),
            );
            failures.eq(
                &case,
                &format!("{label}.name"),
                expected_name,
                actual.error.class_name(),
            );

            let diagnostic = actual.diagnostic_message("startup");
            let expected_prefix = format!("(startup, {expected_scope} settings) ");

            if expected_name == "SyntaxError" {
                // V8's `JSON.parse` wording — see this file's module docs. Structure only.
                v8_syntax_exemptions += 1;
                failures.eq(
                    &case,
                    &format!("{label}.message is non-empty"),
                    true,
                    !actual.error.to_string().is_empty(),
                );
                failures.eq(
                    &case,
                    &format!("{label} diagnostic prefix"),
                    true,
                    diagnostic.starts_with(&expected_prefix),
                );
                failures.eq(
                    &case,
                    &format!("{label} diagnostic tail is the message"),
                    format!("{expected_prefix}{}", actual.error),
                    diagnostic.clone(),
                );
            } else {
                // TypeError from `"queueMode" in settings`: deterministic, asserted verbatim.
                failures.eq(
                    &case,
                    &format!("{label}.message"),
                    expected_message.to_string(),
                    actual.error.to_string(),
                );
            }

            if let Some(expected_diagnostic) = expected_diagnostics.get(index) {
                failures.eq(
                    &case,
                    &format!("diagnostics[{index}].type"),
                    field(expected_diagnostic, "type").as_str().expect("`type`"),
                    "warning",
                );
                if expected_name != "SyntaxError" {
                    failures.eq(
                        &case,
                        &format!("diagnostics[{index}].message"),
                        field(expected_diagnostic, "message")
                            .as_str()
                            .expect("`message`")
                            .to_string(),
                        diagnostic,
                    );
                }
            }
        }

        // `drainErrorsIsIdempotent`: the queue is emptied by the drain.
        if field(record, "drainErrorsIsIdempotent") == &Value::Bool(true) {
            failures.eq(
                &case,
                "drainErrors is idempotent",
                0,
                manager.drain_errors().len(),
            );
        }
    }

    assert_eq!(
        replayed, 12,
        "should replay all 12 SettingsManager.fromStorage records"
    );
    assert_eq!(
        v8_syntax_exemptions, 5,
        "exactly five queued errors (across four records — `malformed:both-scopes-unparseable` \
         queues two) carry V8 `JSON.parse` wording that serde_json cannot reproduce; a change \
         here means the exemption widened or narrowed"
    );
    failures.assert_clean(replayed);
}

/// `JSON.stringify(mergedSettings, null, 2)` (`settings-manager.ts:605`).
///
/// The fixture records no file bytes, so the oracle for this one is the TS call itself: a
/// two-space indent, `": "` after each key, and **no trailing newline** — the same convention
/// `auth.rs` pins for `auth.json`. It also pins that a modified field which is `undefined`
/// disappears from the file, as `JSON.stringify` drops `undefined` values.
#[test]
fn settings_json_is_written_with_a_two_space_indent() {
    let mut settings = SettingsMap::new();
    settings.insert("theme".to_string(), Value::String("dark".to_string()));
    let mut terminal = SettingsMap::new();
    terminal.insert("showImages".to_string(), Value::Bool(false));
    terminal.insert("imageWidthCells".to_string(), Value::from(80));
    settings.insert("terminal".to_string(), Value::Object(terminal));
    settings.insert(
        "extensions".to_string(),
        Value::Array(vec![Value::String("/a.ts".to_string())]),
    );
    settings.insert("empty".to_string(), Value::Object(SettingsMap::new()));
    settings.insert(
        "dropped".to_string(),
        pirust_coding_agent::settings::js_undefined(),
    );

    assert_eq!(
        json_stringify(&Value::Object(settings)),
        concat!(
            "{\n",
            "  \"theme\": \"dark\",\n",
            "  \"terminal\": {\n",
            "    \"showImages\": false,\n",
            "    \"imageWidthCells\": 80\n",
            "  },\n",
            "  \"extensions\": [\n",
            "    \"/a.ts\"\n",
            "  ],\n",
            "  \"empty\": {}\n",
            "}",
        ),
    );
}

/// The whole write path end to end: a legacy file is migrated, only the modified field is
/// rewritten, the user's key order survives, and the file keeps its two-space form.
///
/// Mirrors the fixture's `edge:legacy-shape-is-migrated-on-load` record (which pins the
/// in-memory halves) and extends it to the bytes `persistScopedSettings` emits (`:578-607`).
#[test]
fn saving_a_field_migrates_and_preserves_the_files_own_key_order() {
    let storage = std::sync::Arc::new(InMemorySettingsStorage::with_contents(
        Some("{\n  \"queueMode\": \"all\",\n  \"customKeyPirustDoesNotKnow\": 1\n}".to_string()),
        None,
    ));
    let mut manager =
        SettingsManager::from_storage(storage.clone(), SettingsManagerCreateOptions::default());
    assert!(manager.drain_errors().is_empty());

    manager.set_default_model("claude-opus-4");

    assert_eq!(
        storage.contents(SettingsScope::Global).as_deref(),
        Some(concat!(
            "{\n",
            // `queueMode` was migrated away on the write-merge, `steeringMode` took its
            // place at the END of the key order, and the unknown key survived untouched.
            "  \"customKeyPirustDoesNotKnow\": 1,\n",
            "  \"steeringMode\": \"all\",\n",
            "  \"defaultModel\": \"claude-opus-4\"\n",
            "}",
        )),
    );
    assert!(manager.drain_errors().is_empty());
}
