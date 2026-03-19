use std::collections::HashSet;
use crate::status::{Content, Return};

#[derive(Copy, Clone, Debug)]
pub enum GetRepoFileError{
    ReadConfig,
    ParseConfig,

    NotFound,

    OpenFile,
    ReadDirectory,
    ReadDirectoryEntry,
    ReadDirectoryEntryNonUTF8Name,
    ReadDirectoryEntryFileType,

    Panicked,

    InvalidUTF8,
    BadRequestPath,

    UpstreamRequestError,
    UpstreamBodyReadError,
    UpstreamStatus,
    UpstreamFileTooLarge,
    #[cfg(feature = "put")]
    PutFileTooLarge,

    FileCreateFailed,
    FileWriteFailed,
    FileFlushFailed,
    FileSeekFailed,
    FileLockFailed,
    FileStartsWithDot,
}
impl GetRepoFileError {
    pub fn to_return(self) -> Return {
        Return {
            status: self.get_status_code(),
            content: self.get_err_content(),
            content_type: actix_web::http::header::ContentType::plaintext(),
            header_map: None,
        }
    }
    pub const fn get_err_content(self) -> Content {
        Content::Str(self.get_err())
    }
    pub const fn get_err(self) -> &'static str {
        match self {
            Self::ReadConfig => "Error reading repo config",
            Self::ParseConfig => "Error parsing repo config",
            Self::NotFound => "File or Directory could not be found",
            Self::OpenFile => "Error whilst opening file",
            Self::ReadDirectory => "Error whist reading directory",
            Self::ReadDirectoryEntry => "Error whist reading directory entries",
            Self::ReadDirectoryEntryNonUTF8Name => "Error: directory contains entries with non UTF-8 names",
            Self::ReadDirectoryEntryFileType => "Error: failed to get the file-type of the directory entry",
            Self::Panicked => "Error: implementation panicked",
            Self::InvalidUTF8 => "Error: request path included invalid utf-8 characters",
            Self::BadRequestPath => "Error: Request Path failed sanity checks",
            Self::UpstreamRequestError => "Error: Failed to send a request to the Upstream",
            Self::UpstreamBodyReadError => "Error: Failed to read the response of the Upstream",
            Self::FileCreateFailed => "Error: Failed to create a file to write the upstream's response into",
            Self::FileWriteFailed => "Error: Failed to write to a local file",
            Self::FileFlushFailed => "Error: Failed to flush a local file",
            Self::FileSeekFailed => "Error: Failed to seek a local file",
            Self::FileLockFailed => "Error: Failed to lock a local file",
            Self::UpstreamStatus => "Upstream repo responded with a non 200 status code",
            Self::UpstreamFileTooLarge => "The file from the remote is too Large.",
            #[cfg(feature = "put")]
            Self::PutFileTooLarge => "The file is too Large.",
            Self::FileStartsWithDot => "Error: Refusing to contact upstream about files, which start with a '.'",
        }
    }

    //noinspection RsReplaceMatchExpr - Replacement suggestion makes function non-const
    pub const fn get_status_code(self) -> actix_web::http::StatusCode {
        match self.allowed_status_codes_slice().first().copied() {
            Some(v) => v,
            None => actix_web::http::StatusCode::INTERNAL_SERVER_ERROR
        }
    }
    pub fn allowed_status_codes(self) -> HashSet<actix_web::http::StatusCode> {
        HashSet::from_iter(self.allowed_status_codes_slice().into_iter().copied())
    }
    pub const fn allowed_status_codes_slice(self) -> &'static [actix_web::http::StatusCode] {
        match self {
            Self::ReadConfig =>                     &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::ParseConfig =>                    &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::NotFound =>                       &[actix_web::http::StatusCode::NOT_FOUND, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::OpenFile =>                       &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::ReadDirectory =>                  &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::ReadDirectoryEntry =>             &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::ReadDirectoryEntryNonUTF8Name =>  &[actix_web::http::StatusCode::BAD_REQUEST, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::ReadDirectoryEntryFileType =>     &[actix_web::http::StatusCode::BAD_REQUEST, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::Panicked =>                       &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::InvalidUTF8 =>                    &[actix_web::http::StatusCode::BAD_REQUEST, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::BadRequestPath =>                 &[actix_web::http::StatusCode::BAD_REQUEST, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::UpstreamRequestError =>           &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::UpstreamBodyReadError =>          &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::FileCreateFailed =>               &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::FileWriteFailed =>                &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::FileFlushFailed =>                &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::FileSeekFailed =>                 &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::FileLockFailed =>                 &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::UpstreamStatus =>                 &[actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::UpstreamFileTooLarge =>           &[actix_web::http::StatusCode::INSUFFICIENT_STORAGE, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            #[cfg(feature = "put")]
            Self::PutFileTooLarge =>                &[actix_web::http::StatusCode::PAYLOAD_TOO_LARGE, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
            Self::FileStartsWithDot =>              &[actix_web::http::StatusCode::BAD_REQUEST, actix_web::http::StatusCode::NOT_FOUND, actix_web::http::StatusCode::INTERNAL_SERVER_ERROR],
        }
    }
}