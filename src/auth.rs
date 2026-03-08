use std::time::Instant;
use crate::status::{Content, Return};

#[derive(Debug)]
pub struct BasicAuthentication {
    pub username: String,
    pub password: String,
    pub duration: std::time::Duration,
}

impl actix_web::FromRequest for BasicAuthentication {
    type Error = Return;
    type Future = core::future::Ready<Result<Self, Self::Error>>;

    fn from_request(request: &actix_web::HttpRequest, _: &mut actix_web::dev::Payload) -> Self::Future {
        let start = Instant::now();
        let request = match request.headers().get("Authorization") {
            Some(v) => v,
            None => return core::future::ready(Err(Return{
                status: actix_web::http::StatusCode::FORBIDDEN,
                content: Content::Str("Got a request, which needs authorization, but didn't provide any"),
                content_type: actix_web::http::header::ContentType::plaintext(),
                header_map: Default::default(),
            }))
        };
        let request = match request.to_str() {
            Ok(v) => v,
            Err(_) => return core::future::ready(Err(Return{
                status: actix_web::http::StatusCode::BAD_REQUEST,
                content: Content::Str("The Authorization header contains invalid characters"),
                content_type: actix_web::http::header::ContentType::plaintext(),
                header_map: Default::default(),
            }))
        };
        let auth = match request.strip_prefix("Basic ") {
            Some(v) => v,
            None => return core::future::ready(Err(Return{
                status: actix_web::http::StatusCode::BAD_REQUEST,
                content: Content::Str("Got an Authorization header with something other than 'Basic' type auth"),
                content_type: actix_web::http::header::ContentType::plaintext(),
                header_map: Default::default(),
            }))
        };
        let auth = match data_encoding::BASE64.decode(auth.as_bytes()) {
            Ok(v) => v,
            Err(err) => {
                tracing::error!("Request with Basic Authorization header, but invalid Base64: {err}");
                return core::future::ready(Err(Return{
                    status: actix_web::http::StatusCode::BAD_REQUEST,
                    content: Content::Str("Got an Basic Authorization header with invalid Base64"),
                    content_type: actix_web::http::header::ContentType::plaintext(),
                    header_map: Default::default(),
                }))
            }
        };
        let auth = match String::from_utf8(auth) {
            Ok(v) => v,
            Err(err) => {
                tracing::error!("Request with Basic Authorization header and valid Base64, but the contained bytes were invalid UTF-8: {err}");
                return core::future::ready(Err(Return{
                    status: actix_web::http::StatusCode::BAD_REQUEST,
                    content: Content::Str("Request with Basic Authorization header and valid Base64, but the contained bytes were invalid UTF-8"),
                    content_type: actix_web::http::header::ContentType::plaintext(),
                    header_map: Default::default(),
                }))
            }
        };
        let (username, password) = match auth.split_once(":") {
            Some(v) => v,
            None => {
                tracing::error!("Request with Basic Authorization header and valid Base64 with valid UTF-8 contents, but the contained UTF-8 string did not contain a ':'");
                return core::future::ready(Err(Return{
                    status: actix_web::http::StatusCode::BAD_REQUEST,
                    content: Content::Str("Request with Basic Authorization header and valid Base64 with valid UTF-8 contents, but the contained UTF-8 string did not contain a ':'"),
                    content_type: actix_web::http::header::ContentType::plaintext(),
                    header_map: Default::default(),
                }))
            }
        };
        let username = username.to_owned();
        let password = password.to_owned();

        core::future::ready(Ok(Self{
            username,
            password,
            duration: Instant::now() - start,
        }))
    }

}