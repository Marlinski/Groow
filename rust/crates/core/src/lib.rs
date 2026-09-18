//! The core: the only writer of state, the scheduler, and the API everything else speaks to.
pub mod paths;
pub mod brain;
pub mod config;
pub mod codec;
pub mod conn;
pub mod db;
pub mod hub;
pub mod peer;
pub mod server;
pub mod spawn;
pub mod store;
