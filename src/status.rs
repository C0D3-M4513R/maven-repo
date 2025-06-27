use std::borrow::Cow;
use std::io::Cursor;
use rocket::http::HeaderMap;
use rocket::{Request, Response};
use rocket::response::Responder;

pub enum Return {
    Status(rocket::http::Status),
    // Json((rocket::http::Status, TypedContent<rocket::serde::json::Value>)),
    Content{
        status: rocket::http::Status,
        content: Cow<'static, [u8]>,
        content_type: rocket::http::ContentType,
        header_map: HeaderMap<'static>,
    },
    Redirect(rocket::response::Redirect),
}

impl<'r, 'o:'r> Responder<'r, 'o> for Return {
    fn respond_to(self, request: &'r Request<'_>) -> rocket::response::Result<'o> {
        match self {
            Return::Status(status) => {
                Ok(Response::build()
                    .status(status)
                    .finalize()
                )
            }
            Return::Content { status, content, content_type, header_map } => {
                let mut response = Response::build();
                response
                    .status(status)
                    .sized_body(content.len(), Cursor::new(content));
                for header in header_map.into_iter() {
                    response.header(header);
                }
                response.header(content_type);
                Ok(response.finalize())
            }
            Return::Redirect(redirect) => redirect.respond_to(request)
        }
    }
}