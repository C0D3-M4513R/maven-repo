use std::io::Cursor;
use rocket::http::HeaderMap;
use rocket::{Request, Response};
use futures::TryStreamExt;
use rocket::response::Responder;
use tokio_util::compat::FuturesAsyncReadCompatExt;

pub struct Return {
    pub status: rocket::http::Status,
    pub content: Content,
    pub content_type: rocket::http::ContentType,
    pub header_map: HeaderMap<'static>,
}

pub enum Content {
    File(tokio::fs::File),
    Response(reqwest::Response),
    Str(&'static str),
    String(String),
    ByteSlice(&'static [u8]),
    ByteVec(Vec<u8>),
}
impl Content {
    fn fill_response(self, response: &mut rocket::response::Builder) {
        match self {
            Content::File(file) => response.sized_body(None, file),
            Content::Response(upstream_response) => response.streamed_body(upstream_response.bytes_stream().map_err(std::io::Error::other).into_async_read().compat()),
            Content::Str(data) => response.sized_body(Some(data.len()), Cursor::new(data)),
            Content::String(data) => response.sized_body(Some(data.len()), Cursor::new(data)),
            Content::ByteSlice(data) => response.sized_body(Some(data.len()), Cursor::new(data)),
            Content::ByteVec(data) => response.sized_body(Some(data.len()), Cursor::new(data)),
        };
    }
}

impl<'r, 'o:'r> Responder<'r, 'o> for Return {
    fn respond_to(self, _: &'r Request<'_>) -> rocket::response::Result<'o> {
        let mut response = Response::build();
        response.status(self.status);
        for header in self.header_map.into_iter() {
            response.header(header);
        }
        response.header(self.content_type);
        self.content.fill_response(&mut response);
        Ok(response.finalize())
    }
}