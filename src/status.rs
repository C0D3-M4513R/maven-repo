use futures::StreamExt;

#[derive(Debug)]
pub struct Return {
    pub status: actix_web::http::StatusCode,
    pub content: Content,
    pub content_type: actix_web::http::header::ContentType,
    pub header_map: Option<actix_web::http::header::HeaderMap>,
}

#[derive(Debug)]
pub enum Content {
    Mmap(memmap2::Mmap),
    Response(reqwest::Response),
    Str(&'static str),
    String(String),
    None,
}
impl actix_web::body::MessageBody for Content {
    type Error = std::io::Error;

    fn size(&self) -> actix_web::body::BodySize {
        use actix_web::body::BodySize;
        match &self {
            Self::Mmap(map) => BodySize::Sized(map.len() as u64),
            Self::Response(resp) => {
                let length = match resp.headers().get(reqwest::header::CONTENT_LENGTH) {
                    Some(v) => v,
                    None => return BodySize::Stream
                };
                let length = match length.to_str(){
                    Ok(v) => v,
                    Err(_) => return BodySize::Stream,
                };
                let length = match u64::from_str_radix(length, 10){
                    Ok(v) => v,
                    Err(_) => return BodySize::Stream,
                };
                BodySize::Sized(length)
            },
            Self::Str(s) => BodySize::Sized(s.len() as u64),
            Self::String(s) => BodySize::Sized(s.len() as u64),
            Self::None => BodySize::None,
        }
    }

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Result<actix_web::web::Bytes, Self::Error>>> {
        use actix_web::web::Bytes;
        match core::mem::replace(&mut*self, Self::None) {
            Self::Mmap(map) => std::task::Poll::Ready(Some(Ok(Bytes::from_owner(map)))),
            Self::Response(resp) => resp.bytes_stream().poll_next_unpin(cx).map_err(::std::io::Error::other),
            Self::Str(s) => std::task::Poll::Ready(Some(Ok(Bytes::from_static(s.as_bytes())))),
            Self::String(s) => std::task::Poll::Ready(Some(Ok(Bytes::from(s)))),
            Self::None => std::task::Poll::Ready(None),
        }
    }

    fn try_into_bytes(self) -> Result<actix_web::web::Bytes, Self>
    where
        Self: Sized
    {
        use actix_web::web::Bytes;
        match self {
            Self::Mmap(map) => Ok(Bytes::from_owner(map)),
            Self::Response(_) => Err(self),
            Self::Str(s) => Ok(Bytes::from_static(s.as_bytes())),
            Self::String(s) => Ok(Bytes::from(s)),
            Self::None => Ok(Bytes::new()),
        }
    }
}

impl actix_web::Responder for Return {
    type Body = Content;

    fn respond_to(self, _: &actix_web::HttpRequest) -> actix_web::HttpResponse<Self::Body> {
        let mut resp = actix_web::HttpResponse::with_body(self.status, self.content);
        if let Some(header_map) = self.header_map {
            for (name, value) in header_map{
                resp.headers_mut().append(name, value);
            }
        }
        use actix_web::http::header::TryIntoHeaderValue;
        let type_ = self.content_type.essence_str().to_string();
        match self.content_type.try_into_value() {
            Ok(v) => {
                resp.headers_mut().insert(actix_web::http::header::CONTENT_TYPE, v);
            },
            Err(err) => {
                tracing::warn!("Failed to get header for Content-Type: {err} (value: {type_})")
            }
        }
        resp
    }
}

impl std::fmt::Display for Return {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dummy impl")
    }
}

impl actix_web::ResponseError for Return {
    fn status_code(&self) -> actix_web::http::StatusCode {
        self.status
    }
    fn error_response(&self) -> actix_web::HttpResponse<actix_web::body::BoxBody> {
        let mut resp = actix_web::HttpResponse::with_body(self.status, match &self.content{
            Content::Mmap(_) | Content::Response(_) => actix_web::body::BoxBody::new(()),
            Content::Str(s) => actix_web::body::BoxBody::new(*s),
            Content::String(s) => actix_web::body::BoxBody::new(s.clone()),
            Content::None => actix_web::body::BoxBody::new(()),
        });
        if let Some(header_map) = &self.header_map {
            for (name, value) in header_map{
                resp.headers_mut().append(name.clone(), value.clone());
            }
        }
        use actix_web::http::header::TryIntoHeaderValue;
        let type_ = self.content_type.essence_str();
        match self.content_type.clone().try_into_value() {
            Ok(v) => {
                resp.headers_mut().insert(actix_web::http::header::CONTENT_TYPE, v);
            },
            Err(err) => {
                tracing::warn!("Failed to get header for Content-Type: {err} (value: {type_})")
            }
        }
        resp
    }
}