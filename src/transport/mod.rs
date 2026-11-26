//! How boatparty reaches a sandbox: system OpenSSH with a control master.

pub(crate) mod connection;
pub mod forward;
pub mod ssh;

pub(crate) mod bounded;
