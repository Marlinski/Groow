//! The core: the only writer of state, the scheduler, and the API everything else speaks to.
pub mod brain;
pub mod codec;
pub mod config;
pub mod conn;
pub mod db;
pub mod home;
pub mod hub;
pub mod paths;
pub mod peer;
pub mod server;
pub mod spawn;
pub mod store;
