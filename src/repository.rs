use std::collections::HashMap;
use std::time::Duration;
use rocket::http::{ContentType, Status};
use serde_derive::{Deserialize, Serialize};
use crate::auth::BasicAuthentication;
use crate::status::{Content, Return};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repository{
    pub stores_remote_upstream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publicly_readable: Option<bool>,
    #[serde(default)]
    pub upstreams: Vec<Upstream>,
    #[serde(default)]
    pub tokens: HashMap<String, Token>
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Token{
    pub hash: String,
    pub paths: HashMap<String, PathAuthorization>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PathAuthorization{
    pub read: bool,
    pub put: bool,
    pub delete: bool,
}

impl Repository {
    pub fn check_auth(&self, method: rocket::http::Method, auth: Option<BasicAuthentication>, path: &str) -> Result<(), Return> {
        let needs_auth = match method {
            rocket::http::Method::Get => !self.publicly_readable.unwrap_or(true),
            _ => true,
        };
        if needs_auth {
            let auth = match auth {
                None => return Err(crate::UNAUTHORIZED),
                Some(v) => v,
            };
            let token = match self.tokens.get(&auth.username) {
                Some(v) => v,
                None => return Err(crate::UNAUTHORIZED),
            };
            let path = match token.paths.get(path) {
                None => return Err(crate::UNAUTHORIZED),
                Some(v) => v,
            };
            match bcrypt::verify(&auth.password, &token.hash) {
                Ok(true) => {},
                Ok(false) => return Err(crate::UNAUTHORIZED),
                Err(err) => {
                    tracing::error!("Failed to verify password '{}' against hash '{}': {err}", &auth.password, &token.hash);
                    return Err(Return{
                        status: Status::InternalServerError,
                        content: Content::Str("Error validating password"),
                        content_type: ContentType::Text,
                        header_map: Default::default(),
                    });
                }
            }
            if !match method {
                rocket::http::Method::Get => path.read,
                rocket::http::Method::Put => path.put,
                rocket::http::Method::Delete => path.delete,
                _ => false,
            } {
                return Err(crate::FORBIDDEN);
            }
        }
        Ok(())
    }
}