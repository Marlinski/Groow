//! Frames, and the helpers that build them.
//!
//! The types themselves are generated from `nervous_system/proto/wire.proto`; what is written
//! here is only the convenience of making one and reading it back. To change a frame, change
//! the schema.

use pbjson_types::{Struct, Value as PbValue};
use serde_json::Value;

pub use crate::pb::wire::{frame::Of, Code, Event, Failure, Frame, Part, Push, Reply, Request};

/// Longest line accepted. A frame larger than this is a bug or an attack, not a message.
pub const MAX_LINE: usize = 8 * 1024 * 1024;

impl Frame {
    pub fn req(id: u32, op: impl Into<String>, arg: Value) -> Self {
        Frame { of: Some(Of::Req(Request { id, op: op.into(), arg: to_struct(arg) })) }
    }

    pub fn part(id: u32, data: Value) -> Self {
        Frame { of: Some(Of::Part(Part { id, data: to_struct(data) })) }
    }

    pub fn rep(id: u32, ok: Value) -> Self {
        Frame { of: Some(Of::Rep(Reply { id, ok: Some(to_value(ok)) })) }
    }

    pub fn err(id: u32, e: &WireError) -> Self {
        Frame { of: Some(Of::Err(Failure { id, code: e.code() as i32, msg: e.to_string() })) }
    }

    pub fn ev(name: impl Into<String>, t: f64, data: Value) -> Self {
        Frame { of: Some(Of::Ev(Event { name: name.into(), t, data: to_struct(data) })) }
    }

    pub fn push(name: impl Into<String>, data: Value) -> Self {
        Frame { of: Some(Of::Push(Push { name: name.into(), data: to_struct(data) })) }
    }

    /// The request this frame belongs to, if any. Used to route answers to waiters.
    pub fn id(&self) -> Option<u32> {
        match self.of.as_ref()? {
            Of::Req(r) => Some(r.id),
            Of::Part(p) => Some(p.id),
            Of::Rep(r) => Some(r.id),
            Of::Err(e) => Some(e.id),
            Of::Ev(_) | Of::Push(_) => None,
        }
    }

    /// True when this frame closes out its request. A waiter may be dropped after one.
    pub fn is_terminal(&self) -> bool {
        matches!(self.of, Some(Of::Rep(_)) | Some(Of::Err(_)))
    }

    /// The request in this frame: its id, the op, and its arguments as ordinary JSON.
    pub fn request(&self) -> Option<(u32, &str, Value)> {
        match self.of.as_ref()? {
            Of::Req(r) => Some((r.id, r.op.as_str(), arg_of(&r.arg))),
            _ => None,
        }
    }

    /// The answer in this frame, if it is one.
    pub fn ok(&self) -> Option<Value> {
        match self.of.as_ref()? {
            Of::Rep(r) => Some(r.ok.as_ref().map(from_value).unwrap_or(Value::Null)),
            _ => None,
        }
    }

    /// The refusal in this frame: the stable code, and the message for a person.
    pub fn error(&self) -> Option<(&'static str, &str)> {
        match self.of.as_ref()? {
            Of::Err(e) => Some((
                Code::try_from(e.code).unwrap_or(Code::Unspecified).as_str_name(),
                e.msg.as_str(),
            )),
            _ => None,
        }
    }

    /// A piece of an answer still in flight.
    pub fn part_data(&self) -> Option<Value> {
        match self.of.as_ref()? {
            Of::Part(p) => Some(arg_of(&p.data)),
            _ => None,
        }
    }

    /// An event going up: its name and its payload.
    pub fn event(&self) -> Option<(&str, f64, Value)> {
        match self.of.as_ref()? {
            Of::Ev(e) => Some((e.name.as_str(), e.t, arg_of(&e.data))),
            _ => None,
        }
    }

    /// Something the core said unasked: its name and its payload.
    pub fn pushed(&self) -> Option<(&str, Value)> {
        match self.of.as_ref()? {
            Of::Push(p) => Some((p.name.as_str(), arg_of(&p.data))),
            _ => None,
        }
    }

    pub fn is_rep(&self) -> bool {
        matches!(self.of, Some(Of::Rep(_)))
    }

    pub fn is_err(&self) -> bool {
        matches!(self.of, Some(Of::Err(_)))
    }

    /// Encode with the trailing newline.
    pub fn encode(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| {
            // A frame holds only JSON values and plain scalars, so this is unreachable in
            // practice. Degrade to something parseable rather than panicking on a connection.
            r#"{"err":{"id":0,"code":"CODE_INTERNAL","msg":"frame could not be encoded"}}"#.into()
        });
        s.push('\n');
        s
    }

    /// Decode one line. A blank line is not a frame.
    pub fn decode(line: &str) -> Result<Self, ProtoError> {
        let line = line.trim();
        if line.is_empty() {
            return Err(ProtoError::Empty);
        }
        let f: Frame = serde_json::from_str(line).map_err(|e| ProtoError::Malformed(e.to_string()))?;
        if f.of.is_none() {
            return Err(ProtoError::Malformed("no frame in it".into()));
        }
        Ok(f)
    }
}

/// The payload of a request or an event, as ordinary JSON.
pub fn arg_of(s: &Option<Struct>) -> Value {
    match s {
        Some(s) => from_struct(s),
        None => Value::Object(Default::default()),
    }
}

// ------------------------------------------------------------------ conversions
/// The dynamic parts of the protocol are `Struct` in the schema, which is exactly a JSON
/// object. These move between that and `serde_json` at the edges, so the rest of the code can
/// go on treating arguments and event payloads as the free-form things they are.
pub fn to_struct(v: Value) -> Option<Struct> {
    match v {
        Value::Object(m) => Some(Struct {
            fields: m.into_iter().map(|(k, v)| (k, to_value(v))).collect(),
        }),
        Value::Null => None,
        other => Some(Struct {
            fields: [("value".to_string(), to_value(other))].into_iter().collect(),
        }),
    }
}

pub fn to_value(v: Value) -> PbValue {
    use pbjson_types::value::Kind;
    let kind = match v {
        Value::Null => Kind::NullValue(0),
        Value::Bool(b) => Kind::BoolValue(b),
        Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Kind::StringValue(s),
        Value::Array(a) => Kind::ListValue(pbjson_types::ListValue {
            values: a.into_iter().map(to_value).collect(),
        }),
        Value::Object(m) => Kind::StructValue(Struct {
            fields: m.into_iter().map(|(k, v)| (k, to_value(v))).collect(),
        }),
    };
    PbValue { kind: Some(kind) }
}

pub fn from_struct(s: &Struct) -> Value {
    Value::Object(s.fields.iter().map(|(k, v)| (k.clone(), from_value(v))).collect())
}

pub fn from_value(v: &PbValue) -> Value {
    use pbjson_types::value::Kind;
    match &v.kind {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::BoolValue(b)) => Value::Bool(*b),
        // The schema has one number type and it is a double, so an integer that went in comes
        // back as 8.0 unless it is put back the way it arrived. Anything reading an argument
        // as a count would otherwise silently see nothing and fall back to its default.
        Some(Kind::NumberValue(n)) => whole_or_float(*n),
        Some(Kind::StringValue(s)) => Value::String(s.clone()),
        Some(Kind::ListValue(l)) => Value::Array(l.values.iter().map(from_value).collect()),
        Some(Kind::StructValue(s)) => from_struct(s),
    }
}

/// A number as JSON, keeping whole numbers whole.
fn whole_or_float(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        Value::Number(serde_json::Number::from(n as i64))
    } else {
        serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
    }
}

/// The list the schema uses for a tool call, as ordinary JSON and back.
pub fn to_list(v: Value) -> Option<pbjson_types::ListValue> {
    match v {
        Value::Array(a) => Some(pbjson_types::ListValue {
            values: a.into_iter().map(to_value).collect(),
        }),
        Value::Null => None,
        other => Some(pbjson_types::ListValue { values: vec![to_value(other)] }),
    }
}

pub fn from_list(l: &pbjson_types::ListValue) -> Value {
    Value::Array(l.values.iter().map(from_value).collect())
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("empty line")]
    Empty,
    #[error("malformed frame: {0}")]
    Malformed(String),
}

/// Errors the core hands back. The code is stable and machine-readable; the message is for a
/// person reading a log.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("unknown op `{0}`")]
    UnknownOp(String),
    #[error("bad argument: {0}")]
    BadArg(String),
    #[error("{0} is not permitted for this peer")]
    Denied(String),
    #[error("turn {0} is no longer current")]
    StaleEpoch(String),
    #[error("no such {0}: {1}")]
    NotFound(&'static str, String),
    #[error("nothing to do")]
    Idle,
    #[error("{0}")]
    Busy(String),
    #[error("cancelled")]
    Cancelled,
    #[error("internal error: {0}")]
    Internal(String),
}

impl WireError {
    pub fn code(&self) -> Code {
        match self {
            WireError::UnknownOp(_) => Code::UnknownOp,
            WireError::BadArg(_) => Code::BadArg,
            WireError::Denied(_) => Code::Denied,
            WireError::StaleEpoch(_) => Code::StaleEpoch,
            WireError::NotFound(_, _) => Code::NotFound,
            WireError::Idle => Code::Idle,
            WireError::Busy(_) => Code::Busy,
            WireError::Cancelled => Code::Cancelled,
            WireError::Internal(_) => Code::Internal,
        }
    }

    /// The code as it appears on the wire, for a caller that switches on it.
    pub fn code_name(&self) -> &'static str {
        self.code().as_str_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_every_variant() {
        let frames = vec![
            Frame::req(1, "turn.claim", json!({"why": "user"})),
            Frame::part(1, json!({"delta": "hi"})),
            Frame::rep(1, json!({"turn": "abc"})),
            Frame::err(1, &WireError::Idle),
            Frame::ev("tool_call", 1.5, json!({"name": "shell"})),
            Frame::push("cancel", json!({})),
        ];
        for f in frames {
            let line = f.encode();
            assert!(line.ends_with('\n'));
            assert_eq!(Frame::decode(&line).unwrap(), f, "round trip failed for {f:?}");
        }
    }

    #[test]
    fn a_request_without_arguments_parses() {
        let f = Frame::decode(r#"{"req":{"id":3,"op":"hello"}}"#).unwrap();
        assert_eq!(f.id(), Some(3));
    }

    #[test]
    fn something_that_is_not_a_frame_is_an_error_not_a_guess() {
        assert!(Frame::decode(r#"{"nope":{"id":1}}"#).is_err());
        assert!(Frame::decode(r#"{"id":1,"op":"hello"}"#).is_err());
        assert!(matches!(Frame::decode("   "), Err(ProtoError::Empty)));
    }

    #[test]
    fn terminal_and_id_routing() {
        assert!(Frame::rep(1, json!(null)).is_terminal());
        assert!(Frame::err(1, &WireError::Cancelled).is_terminal());
        assert!(!Frame::part(1, json!(null)).is_terminal());
        assert_eq!(Frame::ev("x", 0.0, json!(null)).id(), None);
    }

    #[test]
    fn a_frame_can_be_read_without_taking_it_apart() {
        assert_eq!(Frame::rep(3, json!({"a": 1})).ok().unwrap()["a"], 1);
        let refused = Frame::err(3, &WireError::Denied("quit".into()));
        let (code, msg) = refused.error().unwrap();
        assert_eq!(code, "CODE_DENIED");
        assert!(msg.contains("quit"));

        let ev = Frame::ev("token", 1.0, json!({"delta": "hi"}));
        let (name, _, data) = ev.event().unwrap();
        assert_eq!((name, data["delta"].as_str()), ("token", Some("hi")));

        let req = Frame::req(9, "say", json!({"text": "x"}));
        let (id, op, arg) = req.request().unwrap();
        assert_eq!((id, op, arg["text"].as_str()), (9, "say", Some("x")));

        assert_eq!(Frame::part(1, json!({"delta": "a"})).part_data().unwrap()["delta"], "a");
        let pushed = Frame::push("cancel", json!({}));
        assert_eq!(pushed.pushed().unwrap().0, "cancel");
        assert!(Frame::rep(1, json!(null)).ok().is_some() && Frame::rep(1, json!(null)).is_rep());
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(WireError::StaleEpoch("t".into()).code_name(), "CODE_STALE_EPOCH");
        assert_eq!(WireError::NotFound("thought", "9".into()).code_name(), "CODE_NOT_FOUND");
    }

    #[test]
    fn json_survives_the_trip_through_the_schemas_own_types() {
        let v = json!({"a": 1, "b": [true, null, "x"], "c": {"d": 2.5}});
        assert_eq!(from_struct(&to_struct(v.clone()).unwrap()), v);
    }

    #[test]
    fn a_whole_number_comes_back_whole() {
        // The schema has one number type, a double. Anything reading `{"max_steps": 8}` as a
        // count must not be handed 8.0 and quietly fall back to its default.
        let v = json!({"max_steps": 8, "ratio": 0.5, "big": 9007199254740992i64});
        let back = from_struct(&to_struct(v).unwrap());
        assert_eq!(back["max_steps"].as_u64(), Some(8));
        assert_eq!(back["ratio"].as_f64(), Some(0.5));
        assert!(back["big"].is_number());
    }

    #[test]
    fn arguments_come_back_as_an_object_even_when_there_were_none() {
        let f = Frame::decode(r#"{"req":{"id":1,"op":"hello"}}"#).unwrap();
        if let Some(Of::Req(r)) = &f.of {
            assert_eq!(arg_of(&r.arg), json!({}));
        } else {
            panic!("not a request");
        }
    }
}
