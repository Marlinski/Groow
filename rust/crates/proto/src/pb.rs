//! The generated types.
//!
//! Everything in here comes from `nervous_system/proto`. Nothing is written by hand, and
//! nothing should be: to change a message, change the schema.

pub mod wire {
    include!(concat!(env!("OUT_DIR"), "/groow.wire.rs"));
    include!(concat!(env!("OUT_DIR"), "/groow.wire.serde.rs"));
}

pub mod brain {
    include!(concat!(env!("OUT_DIR"), "/groow.brain.rs"));
    include!(concat!(env!("OUT_DIR"), "/groow.brain.serde.rs"));
}

pub mod records {
    include!(concat!(env!("OUT_DIR"), "/groow.records.rs"));
    include!(concat!(env!("OUT_DIR"), "/groow.records.serde.rs"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_encodes_as_the_schema_says() {
        let f = wire::Frame {
            of: Some(wire::frame::Of::Req(wire::Request {
                id: 7,
                op: "turn.claim".into(),
                arg: None,
            })),
        };
        let j = serde_json::to_string(&f).unwrap();
        assert_eq!(j, r#"{"req":{"id":7,"op":"turn.claim"}}"#, "the wire shape changed");
        let back: wire::Frame = serde_json::from_str(&j).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn a_message_keeps_its_field_names_and_omits_what_is_absent() {
        let m = wire::Message {
            role: "assistant".into(),
            content: Some("water flows".into()),
            ..Default::default()
        };
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"role":"assistant","content":"water flows"}"#);
    }

    #[test]
    fn a_journal_record_is_the_shape_that_is_already_on_disk() {
        let r = records::JournalRecord {
            ts: 1789755947.44,
            role: "user".into(),
            content: Some("how many files are here?".into()),
            kind: Some("user".into()),
            turn: Some("01789755947441-0004".into()),
            ..Default::default()
        };
        let j = serde_json::to_string(&r).unwrap();
        for part in ["\"ts\":", "\"role\":\"user\"", "\"content\":", "\"kind\":\"user\"", "\"turn\":"] {
            assert!(j.contains(part), "{part} missing from {j}");
        }
    }

    #[test]
    fn enums_encode_by_name_so_a_person_can_read_them() {
        let s = records::Signal {
            priority: 0,
            kind: wire::SignalKind::SignalUser as i32,
            text: "hello".into(),
            ts: 1.0,
            meta: None,
            attempts: 0,
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"kind\":\"SIGNAL_USER\""), "{j}");
    }

    #[test]
    fn an_unknown_field_does_not_stop_a_record_being_read() {
        // A record written by a newer version must still be readable by an older one.
        let j = r#"{"role":"user","content":"hi","something_new":42}"#;
        let m: wire::Message = serde_json::from_str(j).unwrap();
        assert_eq!(m.role, "user");
    }

    #[test]
    fn the_brain_answers_in_the_shape_the_schema_describes() {
        let done = brain::GenChunk {
            of: Some(brain::gen_chunk::Of::Done(brain::Generated {
                text: "Paris.".into(), tokens: 2, seconds: 0.4,
            })),
        };
        let j = serde_json::to_string(&done).unwrap();
        assert_eq!(j, r#"{"done":{"text":"Paris.","tokens":2,"seconds":0.4}}"#);
    }
}
