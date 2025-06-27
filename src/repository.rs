use std::time::Duration;
use serde_derive::{Deserialize, Serialize};
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repository{
    pub stores_remote_upstream: bool,
    pub upstreams: Vec<Upstream>
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum Upstream{
    Local(LocalUpstream),
    Remote(RemoteUpstream),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalUpstream{
    pub path: String, 
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteUpstream{
    pub url: String, 
    pub timeout: Duration,
}
