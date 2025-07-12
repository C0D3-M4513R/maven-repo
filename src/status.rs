use std::io::Cursor;
use rocket::http::HeaderMap;
use rocket::{Request, Response};
use futures::TryStreamExt;
use rocket::response::Responder;
use tokio_util::compat::FuturesAsyncReadCompatExt;

#[derive(Debug)]
pub struct Return {
    pub status: rocket::http::Status,
    pub content: Content,
    pub content_type: rocket::http::ContentType,
    pub header_map: Option<HeaderMap<'static>>,
}

#[derive(Debug)]
pub enum Content {
    Mmap(memmap2::Mmap),
    Response(reqwest::Response),
    Str(&'static str),
    String(String),
    None,
}
impl Content {
    fn fill_response(self, response: &mut rocket::response::Builder) {
        match self {
            Content::Mmap(map) => {
                response.sized_body(None, Cursor::new(map));
            },
            Content::Response(upstream_response) => {
                response.streamed_body(upstream_response.bytes_stream().map_err(std::io::Error::other).into_async_read().compat());
            }
            Content::Str(data) => {
                response.sized_body(Some(data.len()), Cursor::new(data));
            }
            Content::String(data) => {
                response.sized_body(Some(data.len()), Cursor::new(data));
            }
            Content::None => {},
        };
    }
}

impl<'r, 'o:'r> Responder<'r, 'o> for Return {
    fn respond_to(self, _: &'r Request<'_>) -> rocket::response::Result<'o> {
        let mut response = Response::build();
        response.status(self.status);
        if let Some(header_map) = self.header_map {
            for header in header_map.into_iter() {
                response.header(header);
            }
        }
        match self.content{
            Content::None => {},
            _ => {
                response.header(self.content_type);
            }
        };
        self.content.fill_response(&mut response);
        response.header(rocket::http::Header::new("X-Powered-By", env!("CARGO_PKG_REPOSITORY")));
        Ok(response.finalize())
    }
}