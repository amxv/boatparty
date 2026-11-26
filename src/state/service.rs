//! Local URLs and sandbox port claims belong to the serve lease.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub ports: Vec<u16>,
    pub forwards: Vec<ServiceForward>,
    pub started: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceForward {
    pub remote_port: u16,
    pub url: String,
}
