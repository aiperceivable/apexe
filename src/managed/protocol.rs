//! Wire messages for the managed session protocol.
//!
//! This module owns the envelope and body shapes from
//! `docs/managed-execution/session-protocol.md#wire-messages`; the transport
//! (socketpair, peer credentials, descriptor hygiene) is a separate concern and
//! is not implemented here. Parsing is strict: one packet is one UTF-8 JSON
//! object with exactly the envelope fields, unknown keys are refused, and a
//! duplicate key is refused before any value is read.

use serde_json::{json, Map, Value};

use super::error::{ManagedError, ManagedErrorCode};
use super::form::FormId;
use super::json::parse_strict;
use super::plan::{CallId, Id};

/// Maximum packet size, including envelope and encoded output.
pub const MAX_PACKET_BYTES: usize = 64 * 1024;

/// One parsed agent request.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub id: Id,
    pub request: Request,
}

/// The four agent request types.
#[derive(Debug, Clone)]
pub enum Request {
    Prepare {
        form: FormId,
        args: Value,
    },
    Run {
        call_id: CallId,
        plan_digest: String,
    },
    Status {
        call_id: CallId,
    },
    Cancel {
        call_id: CallId,
    },
}

impl Envelope {
    /// Parse one packet.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManagedError> {
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(ManagedError::channel(
                ManagedErrorCode::ProtocolInvalid,
                format!("packet exceeds {MAX_PACKET_BYTES} bytes"),
            ));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| {
            ManagedError::channel(ManagedErrorCode::ProtocolInvalid, "packet is not UTF-8")
        })?;
        let value = parse_strict(text).map_err(|error| {
            ManagedError::channel(ManagedErrorCode::ProtocolInvalid, error.message)
        })?;
        let envelope = exact_object(&value, &["v", "type", "id", "body"], "envelope")?;
        let version = envelope["v"]
            .as_u64()
            .ok_or_else(|| protocol_error("v must be an integer"))?;
        if version != 1 {
            return Err(protocol_error(format!(
                "protocol version {version} is not supported"
            )));
        }
        let id = Id::parse(
            envelope["id"]
                .as_str()
                .ok_or_else(|| protocol_error("id must be a string"))?,
        )?;
        let message_type = envelope["type"]
            .as_str()
            .ok_or_else(|| protocol_error("type must be a string"))?;
        let body = envelope["body"]
            .as_object()
            .ok_or_else(|| protocol_error("body must be an object"))?;
        let request = match message_type {
            "prepare" => parse_prepare(body)?,
            "run" => parse_run(body)?,
            "status" => parse_call_id_only(body, "status")?,
            "cancel" => parse_call_id_only(body, "cancel")?,
            other => return Err(protocol_error(format!("unknown message type '{other}'"))),
        };
        Ok(Self { id, request })
    }
}

fn parse_prepare(body: &Map<String, Value>) -> Result<Request, ManagedError> {
    exact_fields(body, &["form", "args"], "prepare")?;
    let form = body["form"]
        .as_str()
        .ok_or_else(|| protocol_error("form must be a string"))?;
    let form = FormId::parse(form)?;
    let args = body["args"].clone();
    if !args.is_object() {
        return Err(protocol_error("args must be an object"));
    }
    Ok(Request::Prepare { form, args })
}

fn parse_run(body: &Map<String, Value>) -> Result<Request, ManagedError> {
    exact_fields(body, &["call_id", "plan_digest"], "run")?;
    let call_id = CallId::parse(
        body["call_id"]
            .as_str()
            .ok_or_else(|| protocol_error("call_id must be a string"))?,
    )?;
    let plan_digest = body["plan_digest"]
        .as_str()
        .ok_or_else(|| protocol_error("plan_digest must be a string"))?;
    if plan_digest.len() != 64 || !plan_digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(protocol_error(
            "plan_digest must be 64 hexadecimal characters",
        ));
    }
    Ok(Request::Run {
        call_id,
        plan_digest: plan_digest.to_ascii_lowercase(),
    })
}

fn parse_call_id_only(body: &Map<String, Value>, what: &str) -> Result<Request, ManagedError> {
    exact_fields(body, &["call_id"], what)?;
    let call_id = CallId::parse(
        body["call_id"]
            .as_str()
            .ok_or_else(|| protocol_error("call_id must be a string"))?,
    )?;
    Ok(if what == "status" {
        Request::Status { call_id }
    } else {
        Request::Cancel { call_id }
    })
}

/// An `error` reply.
pub fn error_reply(id: &Id, error: &ManagedError) -> Value {
    let mut body = json!({
        "code": error.code.as_str(),
        "phase": error.phase,
        "message": error.message,
    });
    if let Some(field) = &error.field {
        body["field"] = json!(field);
    }
    if let Some(rule_id) = &error.rule_id {
        body["rule_id"] = json!(rule_id);
    }
    json!({"v": 1, "type": "error", "id": id.as_str(), "body": body})
}

/// A `state` reply, used by `status` and `cancel`.
pub fn state_reply(
    id: &Id,
    call_id: &CallId,
    state: &str,
    outcome: Option<(String, String, Option<i32>, Option<i32>)>,
) -> Value {
    let mut body = json!({"call_id": call_id.as_str(), "state": state});
    if let Some((status, mutation, exit_code, signal)) = outcome {
        let mut result =
            json!({"call_id": call_id.as_str(), "status": status, "mutation": mutation});
        if let Some(code) = exit_code {
            result["exit_code"] = json!(code);
        }
        if let Some(signal) = signal {
            result["signal"] = json!(signal);
        }
        body["result"] = result;
    }
    json!({"v": 1, "type": "state", "id": id.as_str(), "body": body})
}

fn exact_object<'a>(
    value: &'a Value,
    fields: &[&str],
    what: &str,
) -> Result<&'a Map<String, Value>, ManagedError> {
    let object = value
        .as_object()
        .ok_or_else(|| protocol_error(format!("{what} must be an object")))?;
    exact_fields(object, fields, what)?;
    Ok(object)
}

fn exact_fields(
    object: &Map<String, Value>,
    fields: &[&str],
    what: &str,
) -> Result<(), ManagedError> {
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut wanted = fields.to_vec();
    wanted.sort_unstable();
    if keys != wanted {
        return Err(protocol_error(format!(
            "{what} must carry exactly {}",
            wanted.join(", ")
        )));
    }
    Ok(())
}

fn protocol_error(message: impl Into<String>) -> ManagedError {
    ManagedError::channel(ManagedErrorCode::ProtocolInvalid, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(body: Value) -> Vec<u8> {
        json!({
            "v": 1,
            "type": "prepare",
            "id": "0123456789abcdef0123456789abcdef",
            "body": body
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn test_a_prepare_packet_parses() {
        let envelope = Envelope::parse(&request(json!({
            "form": "cli.cat.file",
            "args": {"path": {"root": "workspace", "path": "a.txt"}}
        })))
        .unwrap();
        assert!(matches!(envelope.request, Request::Prepare { .. }));
        assert_eq!(envelope.id.as_str(), "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn test_an_extra_envelope_field_is_refused() {
        let mut value: Value = serde_json::from_slice(&request(json!({
            "form": "cli.cat.file",
            "args": {}
        })))
        .unwrap();
        value["extra"] = json!(true);
        let error = Envelope::parse(value.to_string().as_bytes())
            .expect_err("unknown envelope fields must be refused");
        assert_eq!(error.code, ManagedErrorCode::ProtocolInvalid);
    }

    #[test]
    fn test_a_duplicate_key_is_refused() {
        let raw = "{\"v\":1,\"type\":\"prepare\",\"id\":\"0123456789abcdef0123456789abcdef\",\"body\":{\"form\":\"cli.cat.file\",\"form\":\"cli.cat.file\",\"args\":{}}}";
        let error = Envelope::parse(raw.as_bytes()).expect_err("duplicate keys are refused");
        assert_eq!(error.code, ManagedErrorCode::ProtocolInvalid);
    }

    #[test]
    fn test_an_unsupported_version_and_unknown_type_are_refused() {
        let mut value: Value = serde_json::from_slice(&request(json!({
            "form": "cli.cat.file",
            "args": {}
        })))
        .unwrap();
        value["v"] = json!(2);
        assert!(Envelope::parse(value.to_string().as_bytes()).is_err());

        let mut value: Value = serde_json::from_slice(&request(json!({
            "form": "cli.cat.file",
            "args": {}
        })))
        .unwrap();
        value["type"] = json!("delete");
        assert!(Envelope::parse(value.to_string().as_bytes()).is_err());
    }

    #[test]
    fn test_an_oversized_packet_is_refused_before_parsing() {
        let bytes = vec![b' '; MAX_PACKET_BYTES + 1];
        let error = Envelope::parse(&bytes).expect_err("oversized packets are refused");
        assert_eq!(error.code, ManagedErrorCode::ProtocolInvalid);
    }

    #[test]
    fn test_an_error_reply_carries_code_and_phase() {
        let id = Id::parse("0123456789abcdef0123456789abcdef").unwrap();
        let error = ManagedError::planning(ManagedErrorCode::ResourceDenied, "no").with_rule("r1");
        let reply = error_reply(&id, &error);
        assert_eq!(reply["body"]["code"], "RESOURCE_DENIED");
        assert_eq!(reply["body"]["phase"], "planning");
        assert_eq!(reply["body"]["rule_id"], "r1");
    }
}
