use std::time::Instant;
use base64::Engine;
use rocket::http::Status;
use rocket::Request;
use rocket::request::{FromRequest, Outcome};
use crate::status::{Content, Return};

#[derive(Debug)]
pub struct BasicAuthentication {
    pub username: String,
    pub password: String,
    pub duration: std::time::Duration,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for BasicAuthentication {
    type Error = Return;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Return> {
        let start = Instant::now();
        let request = match request.headers().get("Authorization").next() {
            None => return Outcome::Forward(Status::Forbidden),
            Some(v) => v,
        };
        let auth = match request.strip_prefix("Basic ") {
            None => return Outcome::Error((Status::BadRequest, Return{
                status: Status::BadRequest,
                content: Content::Str("Got an Authorization header with something other than 'Basic' type auth"),
                content_type: rocket::http::ContentType::Plain,
                header_map: Default::default(),
            })),
            Some(v) => v,
        };
        let auth = match base64::engine::general_purpose::STANDARD.decode(auth) {
            Ok(v) => v,
            Err(err) => {
                tracing::error!("Request with Basic Authorization header, but invalid Base64: {err}");
                return Outcome::Error((Status::BadRequest, Return{
                    status: Status::BadRequest,
                    content: Content::Str("Got an Basic Authorization header with invalid Base64"),
                    content_type: rocket::http::ContentType::Plain,
                    header_map: Default::default(),
                }))
            }
        };
        let auth = match String::from_utf8(auth) {
            Ok(v) => v,
            Err(err) => {
                tracing::error!("Request with Basic Authorization header and valid Base64, but the contained bytes were invalid UTF-8: {err}");
                return Outcome::Error((Status::BadRequest, Return{
                    status: Status::BadRequest,
                    content: Content::Str("Request with Basic Authorization header and valid Base64, but the contained bytes were invalid UTF-8"),
                    content_type: rocket::http::ContentType::Plain,
                    header_map: Default::default(),
                }))
            }
        };
        let (username, password) = match auth.split_once(":") {
            Some(v) => v,
            None => {
                tracing::error!("Request with Basic Authorization header and valid Base64 with valid UTF-8 contents, but the contained UTF-8 string did not contain a ':'");
                return Outcome::Error((Status::BadRequest, Return{
                    status: Status::BadRequest,
                    content: Content::Str("Request with Basic Authorization header and valid Base64 with valid UTF-8 contents, but the contained UTF-8 string did not contain a ':'"),
                    content_type: rocket::http::ContentType::Plain,
                    header_map: Default::default(),
                }))
            }
        };
        let username = username.to_owned();
        let password = password.to_owned();

        Outcome::Success(Self{
            username,
            password,
            duration: Instant::now() - start,
        })
    }
}