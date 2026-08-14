//! Byte oracle for [`pirust_coding_agent::auth`] against real Pi.
//!
//! Fixture: `tests/fixtures/pi/cli/auth.json.cases.jsonl`, 11 records captured by executing
//! Pi's own `core/auth-storage.ts` — api-key entries, OAuth-token entries, both together,
//! provider ordering, no-op writes, deletes, `$VAR` resolution, a corrupted file, and
//! round-trips through Pi's own writer. **Every expectation below is a literal from that
//! file**; nothing is re-derived here, and [`record`] re-asserts the record count on every
//! call so a shrunken fixture fails loudly instead of silently passing.
//!
//! What the byte assertions pin, and the mutation each one catches:
//!
//! | property | mutation that must fail |
//! |---|---|
//! | two-space indent | 4-space indent |
//! | no trailing newline | append `"\n"` |
//! | provider order = first-write order | sort providers alphabetically |
//! | `type` before `key` inside an entry | emit `key` first |
//!
//! # The `0600` caveat (unverifiable on this platform)
//!
//! Pi writes `auth.json` with `{ mode: 0o600 }` and re-applies `chmodSync(path, 0o600)`
//! (`core/auth-storage.ts:21,86-87,131-132`). Windows cannot express POSIX permissions, so
//! **every** record of this fixture reports `"mode":"0666"` with `"modeMeaningful":false`
//! and `"platform":"win32"`: the capture simply could not observe the mode. Therefore
//! [`assert_credential_file_mode`] asserts `0600` on unix — keyed off the record's
//! `modeMeaningful` flag, so a re-derived fixture is compared against its own literal — and
//! is a no-op on windows, where no mode assertion is possible in either direction. Nothing
//! here ever asserts `0666`. **This fixture should be re-derived on Linux or macOS to pin
//! the mode properly.**
//!
//! # No `std::env::set_var`
//!
//! Record 8 was captured with `ORACLE_AUTH_KEY="sk-from-env"` in the environment.
//! `process.env` is a value here — `ProcessEnv` — so the record is replayed by constructing
//! it, never by mutating the process environment (global, and would race the other tests in
//! this binary). Everything that touches disk uses `tempfile`, so the user's real
//! `~/.pirust` is never read or written.

use std::fs;
use std::path::{Path, PathBuf};

use pirust_coding_agent::auth::{
    read_stored_credential, serialize_storage_data, AuthStorage, AuthStorageData, Credential,
    FileAuthStorageBackend, ProcessEnv, ProviderEnv,
};
use serde_json::Value;

/// The 11 record names, in fixture order.
const RECORD_NAMES: [&str; 11] = [
    "create:constructor-materialises-an-empty-auth.json",
    "write:api-key-entry",
    "write:oauth-token-entry",
    "write:both-kinds-provider-order-is-insertion-order",
    "write:overwriting-an-existing-provider-keeps-its-POSITION",
    "write:callback-returning-undefined-does-NOT-write",
    "delete:removes-a-provider-and-rewrites-unconditionally",
    "read:api-key-values-go-through-resolveConfigValue",
    "roundtrip:writer-output-reparsed-and-reloaded",
    "malformed:reload-keeps-the-last-valid-snapshot",
    "inMemory:same-serialisation-without-a-file",
];

// =============================================================================
// Fixture plumbing
// =============================================================================

/// Every record, in fixture order (`CARGO_MANIFEST_DIR` = `crates/pirust-coding-agent`).
fn records() -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/cli/auth.json.cases.jsonl");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("record {}: {e}", i + 1))
        })
        .collect()
}

/// One record by name, re-asserting the record count so a shrunken fixture fails in *every*
/// test rather than only in [`fixture_has_all_eleven_records`].
fn record(name: &str) -> Value {
    let all = records();
    assert_eq!(
        all.len(),
        RECORD_NAMES.len(),
        "fixture must hold all {} records",
        RECORD_NAMES.len()
    );
    all.into_iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("fixture record {name:?} is missing"))
}

/// `JSON.stringify(value)` — compact, and order-preserving in both directions (the crate
/// enables `serde_json/preserve_order`), so comparing these strings pins key **order** as
/// well as content.
fn compact(value: &Value) -> String {
    serde_json::to_string(value).expect("serialize fixture value")
}

/// The same, for anything the port produces.
fn compact_of<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serialize port value")
}

/// Report the first byte at which the produced file diverges from the captured one.
fn first_diff(want: &str, got: &str) -> String {
    let (wb, gb) = (want.as_bytes(), got.as_bytes());
    let mut i = 0;
    while i < wb.len().min(gb.len()) && wb[i] == gb[i] {
        i += 1;
    }
    format!(
        "first diff at byte {i}\n  expected …{:?}\n  actual   …{:?}",
        &want[i..want.len().min(i + 48)],
        &got[i..got.len().min(i + 48)],
    )
}

/// A fresh store over `path`, with the real environment (only record 8 needs otherwise).
fn store_at(path: &Path) -> AuthStorage<FileAuthStorageBackend> {
    AuthStorage::from_backend(FileAuthStorageBackend::new(path))
}

/// Write a captured `fileBefore`/`fileAfter` block to disk verbatim, so the port starts from
/// exactly the bytes Pi had.
fn seed(path: &Path, block: &Value) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture dir");
    }
    let content = block["content"]
        .as_str()
        .expect("captured file block has string content");
    fs::write(path, content).expect("seed auth.json");
}

/// Assert the file's bytes equal a captured block's `content`, exactly — indent, key order
/// and the **absent** trailing newline included.
fn assert_file_bytes(record: &Value, field: &str, path: &Path) {
    let name = record["name"].as_str().unwrap_or("?");
    let want = record[field]["content"]
        .as_str()
        .unwrap_or_else(|| panic!("{name}: fixture has no {field}.content"));
    assert!(
        !want.ends_with('\n'),
        "{name}: {field} in the fixture must not end with a newline (JSON.stringify emits none)"
    );
    let got = fs::read_to_string(path).unwrap_or_else(|e| panic!("{name}: read {field}: {e}"));
    assert_eq!(
        got,
        want,
        "{name}: {field} bytes differ\n{}",
        first_diff(want, &got)
    );
    assert_credential_file_mode(record, field, path);
}

/// `auth.json` must be `0600` (`core/auth-storage.ts:21,86-87`). Compared against the
/// record's own literal when the capture platform could express it (`modeMeaningful`), and
/// against Pi's source constant otherwise — never against the meaningless `0666`.
#[cfg(unix)]
fn assert_credential_file_mode(record: &Value, field: &str, path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let name = record["name"].as_str().unwrap_or("?");
    let mode = fs::metadata(path)
        .unwrap_or_else(|e| panic!("{name}: stat {field}: {e}"))
        .permissions()
        .mode()
        & 0o777;
    let actual = format!("{mode:04o}");
    if record["modeMeaningful"].as_bool().unwrap_or(false) {
        let want = record[field]["mode"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: fixture has no {field}.mode"));
        assert_eq!(actual, want, "{name}: {field} mode");
    } else {
        assert_eq!(
            actual, "0600",
            "{name}: {field} mode — expected 0600 (auth-storage.ts:21), got {actual}. The \
             fixture's {:?} was captured on {:?} where modes are not meaningful, so this \
             assertion comes from Pi's source; re-derive the fixture on Linux/macOS to pin it.",
            record[field]["mode"], record["platform"]
        );
    }
}

/// No-op on windows: POSIX permissions cannot be observed there, so neither the fixture's
/// captured mode nor the port's intent is verifiable. See the module docs.
#[cfg(not(unix))]
fn assert_credential_file_mode(_record: &Value, _field: &str, _path: &Path) {}

// =============================================================================
// Fixture integrity
// =============================================================================

#[test]
fn fixture_has_all_eleven_records() {
    let all = records();
    assert_eq!(all.len(), 11, "the fixture must hold all 11 records");
    let names: Vec<&str> = all.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, RECORD_NAMES, "record names/order changed");
}

/// Documents the platform caveat rather than hiding it: on windows every record of this
/// capture reports the mode as not meaningful, which is *why* no mode is asserted here.
#[cfg(windows)]
#[test]
fn mode_assertions_are_skipped_on_windows_because_the_capture_could_not_observe_them() {
    for rec in records() {
        if rec["platform"] == "win32" {
            assert_eq!(
                rec["modeMeaningful"].as_bool(),
                Some(false),
                "{}: a win32 capture cannot express POSIX modes",
                rec["name"]
            );
        }
    }
}

// =============================================================================
// Record 1 — construction materialises the file
// =============================================================================

#[test]
fn create_materialises_an_empty_auth_json() {
    let rec = record(RECORD_NAMES[0]);
    let dir = tempfile::tempdir().unwrap();
    // The parent does not exist yet: ensureParentDir must create it (mode 0700, `:38`).
    let path = dir.path().join("agent").join("auth.json");
    assert!(
        rec["fileBefore"].is_null(),
        "fixture: nothing existed before"
    );

    let store = store_at(&path);

    assert_file_bytes(&rec, "fileAfter", &path);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{}",
        "the seed is the literal two bytes `{{}}` (`:44`)"
    );
    assert_eq!(compact_of(&store.list()), compact(&rec["list"]));
}

// =============================================================================
// Record 2 — an api-key entry
// =============================================================================

#[test]
fn modify_writes_an_api_key_entry() {
    let rec = record(RECORD_NAMES[1]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut store = store_at(&path);

    let mut seen: Vec<Value> = Vec::new();
    let returned = store
        .modify("anthropic", |current| {
            seen.push(serde_json::to_value(&current).unwrap());
            Some(Credential::api_key("sk-oracle-1"))
        })
        .expect("modify");

    // `callbackSawCurrent: [null]` — no entry yet, so the callback sees `undefined`.
    assert_eq!(
        compact_of(&seen),
        compact(&rec["callbackSawCurrent"]),
        "{}: callback argument",
        RECORD_NAMES[1]
    );
    assert_eq!(compact_of(&returned), compact(&rec["returned"]));
    assert_file_bytes(&rec, "fileAfter", &path);
    assert_eq!(
        compact_of(&store.read("anthropic")),
        compact(&rec["readBack"])
    );
    assert_eq!(compact_of(&store.list()), compact(&rec["list"]));
}

// =============================================================================
// Record 3 — an OAuth-token entry
// =============================================================================

#[test]
fn modify_writes_a_flat_oauth_entry() {
    let rec = record(RECORD_NAMES[2]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut store = store_at(&path);

    let returned = store
        .modify("anthropic", |_| {
            Some(Credential::oauth(
                "rt-oracle",
                "at-oracle",
                1_730_000_000_000,
            ))
        })
        .expect("modify");

    assert_eq!(compact_of(&returned), compact(&rec["returned"]));
    // No nesting under a "tokens" key, and `expires` stays an integer.
    assert_file_bytes(&rec, "fileAfter", &path);
    assert_eq!(
        compact_of(&store.read("anthropic")),
        compact(&rec["readBack"])
    );
    assert_eq!(compact_of(&store.list()), compact(&rec["list"]));
}

// =============================================================================
// Record 4 — provider order is first-write order, not alphabetical
// =============================================================================

#[test]
fn provider_order_is_insertion_order() {
    let rec = record(RECORD_NAMES[3]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut store = store_at(&path);

    // "openai" is written first and must stay first, even though "anthropic" sorts before it.
    store
        .modify("openai", |_| Some(Credential::api_key("sk-openai")))
        .expect("modify openai");
    store
        .modify("anthropic", |_| {
            Some(Credential::oauth("rt", "at", 1_730_000_000_001))
        })
        .expect("modify anthropic");

    assert_file_bytes(&rec, "fileAfter", &path);
    assert_eq!(
        compact_of(&store.read("openai")),
        compact(&rec["readOpenai"])
    );
    assert_eq!(
        compact_of(&store.read("anthropic")),
        compact(&rec["readAnthropic"])
    );
    assert_eq!(compact_of(&store.list()), compact(&rec["list"]));
}

/// The formatting contract, stated separately so a failure names the property rather than a
/// byte offset. Both halves come from the record-4 literal.
#[test]
fn formatting_is_two_space_indent_with_no_trailing_newline() {
    let rec = record(RECORD_NAMES[3]);
    let want = rec["fileAfter"]["content"].as_str().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut store = store_at(&path);
    store
        .modify("openai", |_| Some(Credential::api_key("sk-openai")))
        .unwrap();
    store
        .modify("anthropic", |_| {
            Some(Credential::oauth("rt", "at", 1_730_000_000_001))
        })
        .unwrap();
    let got = fs::read_to_string(&path).unwrap();

    let line = got.lines().nth(1).expect("a second line");
    assert!(
        line.starts_with("  \"") && !line.starts_with("   "),
        "indent must be exactly two spaces, got {line:?}"
    );
    assert!(!got.ends_with('\n'), "no trailing newline, got {got:?}");
    assert!(!got.ends_with("\r\n"), "no CRLF either, got {got:?}");
    assert_eq!(got, want, "{}", first_diff(want, &got));
    // The same bytes come out of the serializer directly (the single writer).
    let reparsed: AuthStorageData = serde_json::from_str(&got).unwrap();
    assert_eq!(serialize_storage_data(&reparsed).unwrap(), want);
}

// =============================================================================
// Record 5 — overwriting keeps the provider's position
// =============================================================================

#[test]
fn overwriting_a_provider_keeps_its_position() {
    let rec = record(RECORD_NAMES[4]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    seed(&path, &rec["fileBefore"]);
    let mut store = store_at(&path);

    let mut seen: Option<Value> = None;
    store
        .modify("a", |current| {
            seen = Some(serde_json::to_value(&current).unwrap());
            Some(Credential::api_key("1-updated"))
        })
        .expect("modify a");

    // The callback sees the stored value; "a" is updated in place and stays first.
    assert_eq!(
        compact(&seen.unwrap()),
        r#"{"type":"api_key","key":"1"}"#,
        "the callback must see the current credential"
    );
    assert_file_bytes(&rec, "fileAfter", &path);
}

// =============================================================================
// Record 6 — a callback returning undefined does not write
// =============================================================================

#[test]
fn a_callback_returning_none_does_not_write() {
    let rec = record(RECORD_NAMES[5]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    seed(&path, &rec["fileBefore"]);
    let mut store = store_at(&path);

    let returned = store.modify("a", |_| None).expect("modify a");

    // modify() resolves to the UNCHANGED current credential…
    assert_eq!(compact_of(&returned), compact(&rec["returned"]));
    // …and the file is untouched.
    assert_file_bytes(&rec, "fileAfter", &path);
    assert_eq!(
        rec["fileAfter"]["content"], rec["fileBefore"]["content"],
        "fixture: no write happened"
    );
}

// =============================================================================
// Record 7 — delete removes a provider and always rewrites
// =============================================================================

#[test]
fn delete_removes_a_provider_and_rewrites_unconditionally() {
    let rec = record(RECORD_NAMES[6]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    seed(&path, &rec["fileBefore"]);
    let mut store = store_at(&path);

    store.delete("a").expect("delete a");
    assert_file_bytes(&rec, "fileAfterDelete", &path);

    // Deleting an absent provider still rewrites the file.
    store.delete("does-not-exist").expect("delete absent");
    assert_file_bytes(&rec, "fileAfterNoopDelete", &path);
    assert_eq!(compact_of(&store.list()), compact(&rec["list"]));
}

// =============================================================================
// Record 8 — read() resolves api-key values through resolveConfigValue
// =============================================================================

#[test]
fn read_resolves_api_key_values_through_resolve_config_value() {
    let rec = record(RECORD_NAMES[7]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    seed(&path, &rec["fileAfter"]);

    // `env: { ORACLE_AUTH_KEY: "sk-from-env" }` was set for this record only — replayed as a
    // value, never via std::env::set_var.
    let captured_env = rec["env"].as_object().expect("captured env block");
    let process_env = ProcessEnv::from_pairs(
        captured_env
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().expect("string env value").to_string())),
    );
    let store = store_at(&path).with_process_env(process_env);

    let reads = rec["reads"].as_object().expect("reads block");
    assert_eq!(reads.len(), 10, "fixture: 10 read cases");
    for (provider, want) in reads {
        assert_eq!(
            compact_of(&store.read(provider)),
            compact(want),
            "read({provider:?}): expected {} got {}",
            compact(want),
            compact_of(&store.read(provider))
        );
    }
    // The file itself is never rewritten by a read.
    assert_file_bytes(&rec, "fileAfter", &path);
    assert_eq!(compact_of(&store.list()), compact(&rec["list"]));
}

// =============================================================================
// Record 9 — the writer's output reparsed and reloaded
// =============================================================================

#[test]
fn writer_output_reparses_and_reloads() {
    let rec = record(RECORD_NAMES[8]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut store = store_at(&path);

    store
        .modify("anthropic", |_| {
            Some(Credential::oauth("rt", "at", 1_730_000_000_002))
        })
        .expect("modify anthropic");
    store
        .modify("openai", |_| Some(Credential::api_key("sk-rt")))
        .expect("modify openai");

    assert_file_bytes(&rec, "fileAfter", &path);

    // JSON.parse of those bytes: same keys, same order, same values.
    let reparsed: AuthStorageData =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).expect("reparse");
    let key_order: Vec<&String> = reparsed.keys().collect();
    assert_eq!(
        compact_of(&key_order),
        compact(&rec["reparsedKeyOrder"]),
        "top-level key order"
    );
    assert_eq!(compact_of(&reparsed), compact(&rec["reparsed"]));

    // A FRESH store over the same file sees the same credentials.
    let fresh = store_at(&path);
    assert_eq!(
        compact_of(&fresh.read("anthropic")),
        compact(&rec["freshStoreReadAnthropic"])
    );
    assert_eq!(
        compact_of(&fresh.read("openai")),
        compact(&rec["freshStoreReadOpenai"])
    );
    assert_eq!(compact_of(&fresh.list()), compact(&rec["freshStoreList"]));

    // …and so does the one-off sync read.
    let one_off = rec["readStoredCredential"]
        .as_object()
        .expect("readStoredCredential block");
    for (provider, want) in one_off {
        assert_eq!(
            compact_of(&read_stored_credential(provider, &path)),
            compact(want),
            "readStoredCredential({provider:?})"
        );
    }
}

// =============================================================================
// Record 10 — a corrupted file keeps the last valid snapshot
// =============================================================================

#[test]
fn reload_keeps_the_last_valid_snapshot() {
    let rec = record(RECORD_NAMES[9]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut store = store_at(&path);

    store
        .modify("a", |_| Some(Credential::api_key("good")))
        .expect("modify a");
    assert_file_bytes(&rec, "fileBefore", &path);

    // Corrupt it externally, exactly as captured, then reload: no error surfaces and the
    // in-memory snapshot is untouched.
    seed(&path, &rec["fileAfter"]);
    store.reload();
    assert_eq!(
        compact_of(&store.read("a")),
        compact(&rec["readAfterCorruption"])
    );

    // readStoredCredential's catch returns undefined instead.
    assert_eq!(
        compact_of(&read_stored_credential("a", &path)),
        compact(&rec["readStoredCredentialAfterCorruption"])
    );
    assert_eq!(
        compact_of(&read_stored_credential(
            "a",
            &dir.path().join("missing.json")
        )),
        compact(&rec["readStoredCredentialOnMissingFile"])
    );
}

// =============================================================================
// Record 11 — the in-memory backend serializes identically
// =============================================================================

#[test]
fn in_memory_store_matches_the_file_serialisation() {
    let rec = record(RECORD_NAMES[10]);
    assert!(rec["fileAfter"].is_null(), "fixture: no file is involved");

    // Seed data straight from the record's own read values, in its own list order.
    let mut data = AuthStorageData::new();
    for info in rec["list"].as_array().expect("list block") {
        let provider = info["providerId"].as_str().unwrap();
        let read_key = match provider {
            "anthropic" => "readAnthropic",
            "openai" => "readOpenai",
            other => panic!("unexpected provider {other:?}"),
        };
        data.insert(provider.to_string(), rec[read_key].clone());
    }

    let memory = AuthStorage::in_memory(&data).expect("in_memory");
    assert_eq!(
        compact_of(&memory.read("anthropic")),
        compact(&rec["readAnthropic"])
    );
    assert_eq!(
        compact_of(&memory.read("openai")),
        compact(&rec["readOpenai"])
    );
    assert_eq!(compact_of(&memory.list()), compact(&rec["list"]));

    // "the identical serialisation the file backend writes" — proven against the file
    // backend rather than a hand-written literal (the file format is pinned by records 2-9).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    let mut file_store = store_at(&path);
    for info in rec["list"].as_array().unwrap() {
        let provider = info["providerId"].as_str().unwrap().to_string();
        let credential: Credential = serde_json::from_value(
            rec[match provider.as_str() {
                "anthropic" => "readAnthropic",
                _ => "readOpenai",
            }]
            .clone(),
        )
        .unwrap();
        file_store
            .modify(&provider, move |_| Some(credential))
            .expect("modify");
    }
    let on_disk = fs::read_to_string(&path).unwrap();
    assert_eq!(
        memory.backend().snapshot().as_deref(),
        Some(on_disk.as_str()),
        "in-memory and file serialisations must be identical"
    );
    assert!(!on_disk.ends_with('\n'));
}

// =============================================================================
// The typed view against the captured entries
// =============================================================================

/// Every credential shape the fixture contains must survive `parse → typed → serialize`
/// byte-identically, which is what makes it safe for `modify` to rewrite the whole file.
#[test]
fn every_captured_entry_round_trips_through_the_typed_credential() {
    let mut seen = 0usize;
    for rec in records() {
        for field in [
            "fileBefore",
            "fileAfter",
            "fileAfterDelete",
            "fileAfterNoopDelete",
        ] {
            let Some(content) = rec[field]["content"].as_str() else {
                continue;
            };
            let Ok(data) = serde_json::from_str::<AuthStorageData>(content) else {
                continue; // record 10's deliberately corrupted file
            };
            // The whole document round-trips through the single writer…
            assert_eq!(
                serialize_storage_data(&data).unwrap(),
                content,
                "{}: {field} did not round-trip",
                rec["name"]
            );
            // …and so does every entry through the typed view.
            for (provider, value) in &data {
                let credential: Credential = serde_json::from_value(value.clone())
                    .unwrap_or_else(|e| panic!("{}: {provider}: {e}", rec["name"]));
                assert_eq!(
                    compact_of(&credential),
                    compact(value),
                    "{}: {provider} lost or reordered a field",
                    rec["name"]
                );
                seen += 1;
            }
        }
    }
    assert!(seen >= 18, "expected the captured entries, saw {seen}");
}

/// `ProviderEnv` keeps its on-disk key order (a `BTreeMap` would alphabetize it), and the
/// `env`-carrying entry from record 8 is the case that would notice.
#[test]
fn provider_env_preserves_key_order() {
    let mut env = ProviderEnv::new();
    env.insert("Z_FIRST".to_string(), Value::String("1".to_string()));
    env.insert("A_SECOND".to_string(), Value::String("2".to_string()));
    let credential = Credential::api_key_with_env("$Z_FIRST", env);
    assert_eq!(
        compact_of(&credential),
        r#"{"type":"api_key","key":"$Z_FIRST","env":{"Z_FIRST":"1","A_SECOND":"2"}}"#
    );
}
