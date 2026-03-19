pub mod mirror;
pub mod split;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub ip: String,
    pub port: u16,
}

impl Target {
    pub fn new(ip: String, port: u16) -> Self {
        Self { ip, port }
    }

    pub fn to_url(&self, endpoint: &str) -> String {
        format!("http://{}:{}{}", self.ip, self.port, endpoint)
    }
}
