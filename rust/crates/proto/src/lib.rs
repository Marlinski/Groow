//! The wire between the core and everything that talks to it.
//!
//! One connection carries everything: request/reply for work the caller needs an answer to,
//! streamed parts for generation, fire-and-forget events going up, and unsolicited pushes
//! coming down. Frames are newline-delimited JSON, discriminated by the `f` field so a
//! malformed or future frame is a clean parse error instead of a silent misread.
//!
//! ```text
//! {"f":"req", "id":7, "op":"turn.claim", "arg":{}}
//! {"f":"part","id":7, "data":{"delta":"hel"}}
//! {"f":"rep", "id":7, "ok":{"turn":"01J..."}}
//! {"f":"err", "id":7, "code":"stale_epoch", "msg":"turn 01J.. is over"}
//! {"f":"ev",  "name":"tool_call", "t":1789.5, "data":{"name":"shell"}}
//! {"f":"push","name":"cancel", "data":{"why":"user"}}
//! ```

pub mod frame;
pub mod ops;
pub mod turn;
pub mod event;

pub use frame::{Frame, ProtoError, WireError};
pub use ops::Op;
pub use turn::{Epoch, TurnContext, TurnId};
