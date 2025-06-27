use std::borrow::Cow;

#[derive(rocket::response::Responder)]
pub enum Return {
    Status(rocket::http::Status),
    // Json((rocket::http::Status, TypedContent<rocket::serde::json::Value>)),
    Content((rocket::http::Status, TypedContent<Cow<'static, [u8]>>)),
    Redirect(rocket::response::Redirect),
}

#[derive(rocket::response::Responder)]
pub struct TypedContent<T> {
    pub(super) content: T,
    pub(super) content_type: rocket::http::ContentType,
}
impl<T> TypedContent<T> {
    pub const fn html(content: T) -> Self {
        Self { content, content_type: rocket::http::ContentType::HTML }
    }
    pub const fn binary(content: T) -> Self {
        Self { content, content_type: rocket::http::ContentType::Binary }
    }
    pub const fn text(content: T) -> Self {
        Self { content, content_type: rocket::http::ContentType::Text }
    }
}