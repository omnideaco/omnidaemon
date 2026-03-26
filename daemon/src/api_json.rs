//! API JSON helpers — explicit serialization for the daemon→client contract.
//!
//! Every type that crosses the daemon API boundary gets a hand-built JSON
//! transform here. This decouples the client-facing API from Rust struct
//! internals, ensures `x::Value` is unwrapped to plain JSON, and keeps field
//! names matching what TypeScript clients expect.
//!
//! **Why not `serde_json::to_value()`?** Because `#[serde(rename)]`,
//! `#[serde(skip_serializing_if)]`, and `#[serde(flatten)]` silently
//! change the JSON shape when someone edits a struct in a dependency crate.
//! Hand-built JSON makes the API contract visible in *this* file.

use std::collections::HashMap;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};

// ── x::Value ↔ plain JSON ──────────────────────────────────────────────

/// Convert `x::Value` to plain JSON for API responses.
///
/// `x::Value` has custom serde that wraps every variant in a single-key
/// object (e.g. `{"string": "hello"}`). TypeScript expects plain JSON
/// values, so we unwrap here.
pub fn x_to_json(v: &x::Value) -> Value {
    if v.is_null() {
        return Value::Null;
    }
    if let Some(b) = v.as_bool() {
        return Value::Bool(b);
    }
    if let Some(i) = v.as_int() {
        return json!(i);
    }
    if let Some(d) = v.as_double() {
        return json!(d);
    }
    if let Some(s) = v.as_str() {
        return Value::String(s.to_string());
    }
    if let Some(dt) = v.as_date() {
        return Value::String(dt.to_rfc3339());
    }
    if let Some(bytes) = v.as_data() {
        return Value::String(BASE64.encode(bytes));
    }
    if let Some(arr) = v.as_array() {
        return Value::Array(arr.iter().map(x_to_json).collect());
    }
    if let Some(dict) = v.as_dictionary() {
        let obj: serde_json::Map<String, Value> = dict
            .iter()
            .map(|(k, v)| (k.clone(), x_to_json(v)))
            .collect();
        return Value::Object(obj);
    }
    // Unreachable for a well-formed x::Value, but safe fallback.
    Value::Null
}

/// Convert plain JSON to `x::Value` for API requests.
///
/// Maps JSON types back to the closest `x::Value` variant. JSON numbers
/// without a fractional part become `Int`, others become `Double`.
pub fn json_to_x(v: &Value) -> x::Value {
    match v {
        Value::Null => x::Value::Null,
        Value::Bool(b) => x::Value::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                x::Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                x::Value::Double(f)
            } else {
                // u64 that doesn't fit i64 — store as Double.
                x::Value::Double(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => x::Value::String(s.clone()),
        Value::Array(arr) => x::Value::Array(arr.iter().map(json_to_x).collect()),
        Value::Object(obj) => {
            let map: HashMap<String, x::Value> = obj
                .iter()
                .map(|(k, v)| (k.clone(), json_to_x(v)))
                .collect();
            x::Value::Dictionary(map)
        }
    }
}

/// Convert an `x::Value` properties map to a plain JSON object.
pub fn x_props_to_json(props: &HashMap<String, x::Value>) -> Value {
    let obj: serde_json::Map<String, Value> = props
        .iter()
        .map(|(k, v)| (k.clone(), x_to_json(v)))
        .collect();
    Value::Object(obj)
}

/// Convert a plain JSON object to an `x::Value` properties map.
pub fn json_to_x_props(obj: &serde_json::Map<String, Value>) -> HashMap<String, x::Value> {
    obj.iter()
        .map(|(k, v)| (k.clone(), json_to_x(v)))
        .collect()
}

// ── Protocol types → API JSON ──────────────────────────────────────────

/// Build JSON for a `ManifestEntry`.
///
/// Renames `created_at` → `created` and `modified_at` → `modified` to
/// match the TypeScript client contract. Accepts an optional `root_id`
/// which is not part of ManifestEntry but expected by the client.
pub fn manifest_entry_json(
    entry: &vault::ManifestEntry,
    root_id: Option<&uuid::Uuid>,
) -> Value {
    json!({
        "id": entry.id.to_string(),
        "path": &entry.path,
        "title": entry.title.as_deref(),
        "extended_type": entry.extended_type.as_deref(),
        "creator": &entry.creator,
        "created": entry.created_at.to_rfc3339(),
        "modified": entry.modified_at.to_rfc3339(),
        "collective_id": entry.collective_id.map(|id| id.to_string()),
        "root_id": root_id.map(|id| id.to_string()),
    })
}

/// Build JSON for a `SearchHit`.
pub fn search_hit_json(hit: &vault::SearchHit) -> Value {
    json!({
        "idea_id": hit.idea_id.to_string(),
        "relevance": hit.relevance,
        "snippet": hit.snippet,
        "title": hit.title,
    })
}

/// Build JSON for an `OmniEvent`.
///
/// Wire protocol type — field names stay as-is (`created_at` remains
/// `created_at` since this is the ORP wire format, an integer timestamp).
pub fn omni_event_json(event: &globe::OmniEvent) -> Value {
    json!({
        "id": event.id,
        "author": event.author,
        "created_at": event.created_at,
        "kind": event.kind,
        "tags": event.tags,
        "content": event.content,
        "sig": event.sig,
    })
}

/// Build JSON for an `OmnibusStatus`.
pub fn omnibus_status_json(status: &omnibus::OmnibusStatus) -> Value {
    json!({
        "has_identity": status.has_identity,
        "pubkey": status.pubkey,
        "display_name": status.display_name,
        "relay_port": status.relay_port,
        "relay_connections": status.relay_connections,
        "relay_url": status.relay_url,
        "discovered_peers": status.discovered_peers,
        "pool_relays": status.pool_relays,
        "has_home_node": status.has_home_node,
        "public_url": status.public_url,
        "http_port": status.http_port,
        "http_url": status.http_url,
    })
}

/// Build JSON for a `RelayHealthSnapshot`.
pub fn relay_health_json(h: &omnibus::RelayHealthSnapshot) -> Value {
    json!({
        "url": h.url,
        "state": h.state,
        "connected_since": h.connected_since.map(|dt| dt.to_rfc3339()),
        "last_activity": h.last_activity.map(|dt| dt.to_rfc3339()),
        "send_count": h.send_count,
        "receive_count": h.receive_count,
        "error_count": h.error_count,
        "average_latency_ms": h.average_latency_ms,
        "score": h.score,
    })
}

/// Build JSON for `StoreStats`.
pub fn store_stats_json(s: &globe::StoreStats) -> Value {
    let by_kind: serde_json::Map<String, Value> = s
        .events_by_kind
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();

    json!({
        "event_count": s.event_count,
        "oldest_event": s.oldest_event,
        "newest_event": s.newest_event,
        "events_by_kind": Value::Object(by_kind),
    })
}

/// Build JSON for a `LogEntry`.
pub fn log_entry_json(l: &omnibus::LogEntry) -> Value {
    json!({
        "timestamp": l.timestamp.to_rfc3339(),
        "level": l.level,
        "module": l.module,
        "message": l.message,
    })
}

/// Build JSON for a `DaemonConfig`.
pub fn daemon_config_json(config: &omnibus::DaemonConfig) -> Value {
    json!({
        "omnibus": {
            "port": config.omnibus.port,
            "bind_all": config.omnibus.bind_all,
            "device_name": config.omnibus.device_name,
            "data_dir": config.omnibus.data_dir.as_ref().map(|p| p.display().to_string()),
            "enable_upnp": config.omnibus.enable_upnp,
            "home_node": config.omnibus.home_node,
        },
        "tower": {
            "enabled": config.tower.enabled,
            "mode": config.tower.mode,
            "name": config.tower.name,
            "seeds": config.tower.seeds,
            "communities": config.tower.communities,
            "announce_interval_secs": config.tower.announce_interval_secs,
            "gospel_interval_secs": config.tower.gospel_interval_secs,
            "gospel_live_interval_secs": config.tower.gospel_live_interval_secs,
            "public_url": config.tower.public_url,
        },
    })
}

/// Convert a full `IdeaPackage` to the API format.
///
/// Flattens the header (extracts `root_id` from `header.content.root_digit_id`,
/// `author` from `header.creator.public_key`), unwraps `x::Value` in digit
/// content and properties, and formats all UUIDs as strings.
pub fn idea_package_json(pkg: &ideas::IdeaPackage) -> Value {
    let header = &pkg.header;

    let digits_obj: serde_json::Map<String, Value> = pkg
        .digits
        .iter()
        .map(|(uuid, digit)| {
            let digit_json = json!({
                "id": digit.id().to_string(),
                "type": digit.digit_type(),
                "content": x_to_json(&digit.content),
                "properties": x_props_to_json(&digit.properties),
                "children": digit.children.as_deref().unwrap_or(&[])
                    .iter()
                    .map(|id| Value::String(id.to_string()))
                    .collect::<Vec<_>>(),
                "author": digit.author(),
                "created": digit.created().to_rfc3339(),
                "modified": digit.modified.to_rfc3339(),
            });
            (uuid.to_string(), digit_json)
        })
        .collect();

    json!({
        "header": {
            "id": header.id.to_string(),
            "root_id": header.content.root_digit_id.to_string(),
            "author": header.creator.public_key,
            "created": header.created.to_rfc3339(),
            "modified": header.modified.to_rfc3339(),
            "extended_type": header.extended_type,
        },
        "digits": Value::Object(digits_obj),
    })
}

// ── Editor types → API JSON ─────────────────────────────────────────

/// Build JSON for a single `SequenceOp`.
pub fn sequence_op_json(op: &x::crdt::sequence::SequenceOp) -> Value {
    match op {
        x::crdt::sequence::SequenceOp::Insert { id, value, after } => json!({
            "type": "insert",
            "id": { "replica_id": id.replica_id, "seq": id.seq },
            "value": value.to_string(),
            "after": after.as_ref().map(|a| json!({
                "replica_id": a.replica_id,
                "seq": a.seq,
            })),
        }),
        x::crdt::sequence::SequenceOp::Delete { id } => json!({
            "type": "delete",
            "id": { "replica_id": id.replica_id, "seq": id.seq },
        }),
    }
}

/// Build JSON for a list of `SequenceOp`s.
pub fn sequence_ops_json(ops: &[x::crdt::sequence::SequenceOp]) -> Value {
    Value::Array(ops.iter().map(sequence_op_json).collect())
}

/// Build JSON for an editor state snapshot (all field texts + version).
pub fn editor_state_json(
    fields: &std::collections::HashMap<
        (uuid::Uuid, String),
        x::crdt::sequence::SequenceRga,
    >,
    version: u64,
) -> Value {
    let fields_obj: serde_json::Map<String, Value> = fields
        .iter()
        .map(|((digit_id, field_name), rga)| {
            let key = format!("{}:{}", digit_id, field_name);
            let val = json!({
                "digit_id": digit_id.to_string(),
                "field": field_name,
                "text": rga.text(),
            });
            (key, val)
        })
        .collect();

    json!({
        "fields": Value::Object(fields_obj),
        "version": version,
    })
}

/// Build JSON for an edit result (ops applied + resulting text + version).
pub fn edit_result_json(
    ops: &[x::crdt::sequence::SequenceOp],
    text: &str,
    version: u64,
) -> Value {
    json!({
        "ops": sequence_ops_json(ops),
        "text": text,
        "version": version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── x::Value conversion tests ──

    #[test]
    fn test_x_null_to_json() {
        assert_eq!(x_to_json(&x::Value::Null), Value::Null);
    }

    #[test]
    fn test_x_bool_to_json() {
        assert_eq!(x_to_json(&x::Value::Bool(true)), Value::Bool(true));
        assert_eq!(x_to_json(&x::Value::Bool(false)), Value::Bool(false));
    }

    #[test]
    fn test_x_int_to_json() {
        assert_eq!(x_to_json(&x::Value::Int(42)), json!(42));
        assert_eq!(x_to_json(&x::Value::Int(-999)), json!(-999));
        assert_eq!(x_to_json(&x::Value::Int(0)), json!(0));
    }

    #[test]
    fn test_x_double_to_json() {
        assert_eq!(x_to_json(&x::Value::Double(3.14)), json!(3.14));
        assert_eq!(x_to_json(&x::Value::Double(0.0)), json!(0.0));
    }

    #[test]
    fn test_x_string_to_json() {
        assert_eq!(
            x_to_json(&x::Value::String("hello".into())),
            json!("hello")
        );
        assert_eq!(
            x_to_json(&x::Value::String(String::new())),
            json!("")
        );
    }

    #[test]
    fn test_x_date_to_json() {
        let dt = chrono::Utc::now();
        let result = x_to_json(&x::Value::Date(dt));
        assert_eq!(result, Value::String(dt.to_rfc3339()));
    }

    #[test]
    fn test_x_data_to_json_base64() {
        let result = x_to_json(&x::Value::Data(vec![0xDE, 0xAD, 0xBE, 0xEF]));
        assert_eq!(result, json!("3q2+7w=="));

        // Empty data.
        assert_eq!(x_to_json(&x::Value::Data(vec![])), json!(""));
    }

    #[test]
    fn test_x_array_to_json() {
        let v = x::Value::Array(vec![
            x::Value::Int(1),
            x::Value::String("two".into()),
            x::Value::Bool(true),
        ]);
        assert_eq!(x_to_json(&v), json!([1, "two", true]));
    }

    #[test]
    fn test_x_dictionary_to_json() {
        let mut map = HashMap::new();
        map.insert("name".to_string(), x::Value::String("test".into()));
        map.insert("count".to_string(), x::Value::Int(5));
        let result = x_to_json(&x::Value::Dictionary(map));
        assert_eq!(result["name"], json!("test"));
        assert_eq!(result["count"], json!(5));
    }

    #[test]
    fn test_x_nested_to_json() {
        let inner = x::Value::Array(vec![x::Value::Null, x::Value::Int(42)]);
        let mut dict = HashMap::new();
        dict.insert("items".to_string(), inner);
        let result = x_to_json(&x::Value::Dictionary(dict));
        assert_eq!(result["items"], json!([null, 42]));
    }

    // ── json_to_x tests ──

    #[test]
    fn test_json_null_to_x() {
        assert!(json_to_x(&Value::Null).is_null());
    }

    #[test]
    fn test_json_bool_to_x() {
        assert_eq!(json_to_x(&json!(true)).as_bool(), Some(true));
        assert_eq!(json_to_x(&json!(false)).as_bool(), Some(false));
    }

    #[test]
    fn test_json_integer_to_x() {
        assert_eq!(json_to_x(&json!(42)).as_int(), Some(42));
        assert_eq!(json_to_x(&json!(-1)).as_int(), Some(-1));
    }

    #[test]
    fn test_json_float_to_x() {
        assert_eq!(json_to_x(&json!(3.14)).as_double(), Some(3.14));
    }

    #[test]
    fn test_json_string_to_x() {
        assert_eq!(json_to_x(&json!("hello")).as_str(), Some("hello"));
    }

    #[test]
    fn test_json_array_to_x() {
        let v = json_to_x(&json!([1, "two", true]));
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_int(), Some(1));
        assert_eq!(arr[1].as_str(), Some("two"));
        assert_eq!(arr[2].as_bool(), Some(true));
    }

    #[test]
    fn test_json_object_to_x() {
        let v = json_to_x(&json!({"name": "test", "count": 5}));
        let dict = v.as_dictionary().unwrap();
        assert_eq!(dict["name"].as_str(), Some("test"));
        assert_eq!(dict["count"].as_int(), Some(5));
    }

    // ── x_props round-trip ──

    #[test]
    fn test_x_props_round_trip() {
        let mut props = HashMap::new();
        props.insert("title".to_string(), x::Value::String("My Note".into()));
        props.insert("count".to_string(), x::Value::Int(3));
        props.insert("visible".to_string(), x::Value::Bool(true));

        let json_obj = x_props_to_json(&props);
        let obj = json_obj.as_object().unwrap();
        let restored = json_to_x_props(obj);

        assert_eq!(restored["title"].as_str(), Some("My Note"));
        assert_eq!(restored["count"].as_int(), Some(3));
        assert_eq!(restored["visible"].as_bool(), Some(true));
    }

    // ── ManifestEntry tests ──

    #[test]
    fn test_manifest_entry_renames_timestamp_fields() {
        let entry = vault::ManifestEntry {
            id: uuid::Uuid::new_v4(),
            path: "Personal/test.idea".to_string(),
            title: Some("Test Note".to_string()),
            extended_type: Some("text".to_string()),
            creator: "cpub1abc".to_string(),
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            collective_id: None,
            header_cache: None,
        };
        let root = uuid::Uuid::new_v4();
        let j = manifest_entry_json(&entry, Some(&root));

        // Renamed fields present.
        assert!(j.get("created").is_some(), "should have 'created'");
        assert!(j.get("modified").is_some(), "should have 'modified'");
        // Original field names absent.
        assert!(j.get("created_at").is_none(), "should NOT have 'created_at'");
        assert!(j.get("modified_at").is_none(), "should NOT have 'modified_at'");
        // root_id injected.
        assert_eq!(j["root_id"], json!(root.to_string()));
        assert_eq!(j["title"], json!("Test Note"));
    }

    #[test]
    fn test_manifest_entry_null_optionals() {
        let entry = vault::ManifestEntry {
            id: uuid::Uuid::new_v4(),
            path: "test.idea".to_string(),
            title: None,
            extended_type: None,
            creator: "cpub1abc".to_string(),
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            collective_id: None,
            header_cache: None,
        };
        let j = manifest_entry_json(&entry, None);
        assert_eq!(j["root_id"], Value::Null);
        assert_eq!(j["title"], Value::Null);
        assert_eq!(j["collective_id"], Value::Null);
    }

    // ── SearchHit test ──

    #[test]
    fn test_search_hit_json_format() {
        let hit = vault::SearchHit {
            idea_id: uuid::Uuid::new_v4(),
            relevance: 0.85,
            snippet: Some("...matching text...".into()),
            title: Some("Found Note".into()),
        };
        let j = search_hit_json(&hit);
        assert_eq!(j["idea_id"], json!(hit.idea_id.to_string()));
        assert_eq!(j["relevance"], json!(0.85));
        assert_eq!(j["snippet"], json!("...matching text..."));
    }

    // ── OmniEvent test ──

    #[test]
    fn test_omni_event_preserves_created_at_as_integer() {
        let event = globe::OmniEvent {
            id: "abc123".into(),
            author: "def456".into(),
            created_at: 1700000000,
            kind: 1,
            tags: vec![vec!["e".into(), "ref123".into()]],
            content: "hello".into(),
            sig: "sig789".into(),
        };
        let j = omni_event_json(&event);
        assert_eq!(j["created_at"], json!(1700000000));
        assert_eq!(j["kind"], json!(1));
        assert_eq!(j["author"], json!("def456"));
    }

    // ── OmnibusStatus test ──

    #[test]
    fn test_omnibus_status_includes_all_fields() {
        let status = omnibus::OmnibusStatus {
            has_identity: true,
            pubkey: Some("cpub1test".into()),
            display_name: Some("Test User".into()),
            relay_port: 4040,
            relay_connections: 3,
            relay_url: "ws://127.0.0.1:4040".into(),
            discovered_peers: 2,
            pool_relays: 5,
            has_home_node: false,
            public_url: None,
            http_port: Some(8080),
            http_url: Some("http://127.0.0.1:8080".into()),
        };
        let j = omnibus_status_json(&status);
        assert_eq!(j["has_identity"], json!(true));
        assert_eq!(j["pubkey"], json!("cpub1test"));
        assert_eq!(j["relay_port"], json!(4040));
        assert_eq!(j["http_port"], json!(8080));
        assert_eq!(j["public_url"], Value::Null);
    }

    // ── RelayHealth test ──

    #[test]
    fn test_relay_health_datetime_as_iso_string() {
        let h = omnibus::RelayHealthSnapshot {
            url: "wss://relay.example.com".into(),
            state: "connected".into(),
            connected_since: Some(chrono::Utc::now()),
            last_activity: None,
            send_count: 42,
            receive_count: 100,
            error_count: 0,
            average_latency_ms: Some(55.5),
            score: 0.9,
        };
        let j = relay_health_json(&h);
        assert!(j["connected_since"].is_string(), "DateTime should be ISO string");
        assert_eq!(j["last_activity"], Value::Null);
        assert_eq!(j["send_count"], json!(42));
    }

    // ── StoreStats test ──

    #[test]
    fn test_store_stats_kind_keys_are_strings() {
        let mut by_kind = HashMap::new();
        by_kind.insert(0, 10);
        by_kind.insert(1, 5);
        let s = globe::StoreStats {
            event_count: 15,
            oldest_event: Some(1600000000),
            newest_event: Some(1700000000),
            events_by_kind: by_kind,
        };
        let j = store_stats_json(&s);
        assert_eq!(j["event_count"], json!(15));
        let obj = j["events_by_kind"].as_object().unwrap();
        assert!(obj.contains_key("0") && obj.contains_key("1"));
    }

    // ── LogEntry test ──

    #[test]
    fn test_log_entry_timestamp_is_iso() {
        let l = omnibus::LogEntry {
            timestamp: chrono::Utc::now(),
            level: "INFO".into(),
            module: Some("omnibus::runtime".into()),
            message: "Started relay server".into(),
        };
        let j = log_entry_json(&l);
        assert!(j["timestamp"].is_string());
        assert_eq!(j["level"], json!("INFO"));
        assert_eq!(j["module"], json!("omnibus::runtime"));
    }

    // ── DaemonConfig test ──

    #[test]
    fn test_daemon_config_nested_structure() {
        let config = omnibus::DaemonConfig {
            omnibus: omnibus::OmnibusSection {
                port: 4040,
                bind_all: false,
                device_name: "Test Device".into(),
                data_dir: None,
                enable_upnp: false,
                home_node: None,
            },
            tower: omnibus::TowerSection::default(),
        };
        let j = daemon_config_json(&config);
        assert_eq!(j["omnibus"]["port"], json!(4040));
        assert_eq!(j["omnibus"]["device_name"], json!("Test Device"));
        assert_eq!(j["omnibus"]["data_dir"], Value::Null);
        assert_eq!(j["tower"]["enabled"], json!(false));
        assert_eq!(j["tower"]["mode"], json!("pharos"));
        assert!(j["tower"]["seeds"].is_array());
    }
}
