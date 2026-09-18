//! Frame encoding. Every byte that crosses the socket is one of these, one per line.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The complete set of things that can appear on the wire.
///
/// `f` is the discriminator and it is never absent: an unknown value fails to parse rather
/// than being mistaken for another frame. That is the whole reason this is a tagged enum and
/// not an untagged one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "f", rename_all = "lowercase")]
pub enum Frame {
    /// Caller asks for something and waits for exactly one terminal reply.
    Req {
        id: u64,
        op: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        arg: Value,
    },
    /// A chunk of an answer still in flight. Zero or more precede the terminal frame.
    Part {
        id: u64,
        data: Value,
    },
    /// Terminal success for `id`.
    Rep {
        id: u64,
        ok: Value,
    },
    /// Terminal failure for `id`.
    Err {
        id: u64,
        code: String,
        msg: String,
    },
    /// Something happened. Nobody replies to it and nobody may block on it.
    Ev {
        name: String,
        t: f64,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        data: Value,
    },
    /// The core telling the peer something it did not ask for.
    Push {
        name: String,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        data: Value,
    },
}

impl Frame {
    /// The request id this frame belongs to, if any. Used to route replies to waiters.
    pub fn id(&self) -> Option<u64> {
        match self {
            Frame::Req { id, .. }
            | Frame::Part { id, .. }
            | Frame::Rep { id, .. }
            | Frame::Err { id, .. } => Some(*id),
            Frame::Ev { .. } | Frame::Push { .. } => None,
        }
    }

    /// True when this frame closes out its request. A waiter may be dropped after one.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Frame::Rep { .. } | Frame::Err { .. })
    }

    pub fn req(id: u64, op: impl Into<String>, arg: Value) -> Self {
        Frame::Req { id, op: op.into(), arg }
    }

    pub fn rep(id: u64, ok: Value) -> Self {
        Frame::Rep { id, ok }
    }

    pub fn err(id: u64, e: &WireError) -> Self {
        Frame::Err { id, code: e.code().to_string(), msg: e.to_string() }
    }

    pub fn ev(name: impl Into<String>, t: f64, data: Value) -> Self {
        Frame::Ev { name: name.into(), t, data }
    }

    pub fn push(name: impl Into<String>, data: Value) -> Self {
        Frame::Push { name: name.into(), data }
    }

    /// Encode with the trailing newline. Serialisation of a `Frame` cannot fail, so this
    /// returns a `String` rather than a `Result`; the `unreachable` arm is only there because
    /// `serde_json` cannot prove it.
    pub fn encode(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| {
            // A Frame holds only JSON values and plain scalars; this is unreachable in
            // practice. Degrade to a parseable error frame rather than panicking on a live
            // connection.
            r#"{"f":"err","id":0,"code":"encode","msg":"frame could not be encoded"}"#.to_string()
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
        serde_json::from_str(line).map_err(|e| ProtoError::Malformed(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("empty line")]
    Empty,
    #[error("malformed frame: {0}")]
    Malformed(String),
}

/// Errors the core hands back across the wire. The code is stable and machine-readable; the
/// message is for a human reading a log.
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
    #[error("busy: {0}")]
    Busy(String),
    #[error("cancelled")]
    Cancelled,
    #[error("internal error: {0}")]
    Internal(String),
}

impl WireError {
    pub fn code(&self) -> &'static str {
        match self {
            WireError::UnknownOp(_) => "unknown_op",
            WireError::BadArg(_) => "bad_arg",
            WireError::Denied(_) => "denied",
            WireError::StaleEpoch(_) => "stale_epoch",
            WireError::NotFound(_, _) => "not_found",
            WireError::Idle => "idle",
            WireError::Busy(_) => "busy",
            WireError::Cancelled => "cancelled",
            WireError::Internal(_) => "internal",
        }
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
            Frame::Part { id: 1, data: json!({"delta": "hi"}) },
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
    fn req_without_arg_parses() {
        let f = Frame::decode(r#"{"f":"req","id":3,"op":"hello"}"#).unwrap();
        assert!(matches!(f, Frame::Req { id: 3, .. }));
    }

    #[test]
    fn unknown_discriminator_is_an_error_not_a_guess() {
        assert!(Frame::decode(r#"{"f":"nope","id":1}"#).is_err());
        assert!(Frame::decode(r#"{"id":1,"op":"hello"}"#).is_err());
        assert!(matches!(Frame::decode("   "), Err(ProtoError::Empty)));
    }

    #[test]
    fn terminal_and_id_routing() {
        assert!(Frame::rep(1, json!(null)).is_terminal());
        assert!(Frame::err(1, &WireError::Cancelled).is_terminal());
        assert!(!Frame::Part { id: 1, data: json!(null) }.is_terminal());
        assert_eq!(Frame::ev("x", 0.0, json!(null)).id(), None);
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(WireError::StaleEpoch("t".into()).code(), "stale_epoch");
        assert_eq!(WireError::NotFound("thought", "9".into()).code(), "not_found");
    }
}
