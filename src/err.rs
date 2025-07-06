use std::borrow::Cow;
use std::collections::HashSet;
use reqwest::StatusCode;
use rocket::http::{ContentType, Status};
use crate::status::{Content, Return};

#[derive(Copy, Clone, Debug)]
pub enum GetRepoFileError{
    OpenConfig,
    ReadConfig,
    ParseConfig,

    NotFound,

    OpenFile,
    ReadDirectory,
    ReadDirectoryEntry,
    ReadDirectoryEntryNonUTF8Name,

    Panicked,

    InvalidUTF8,
    BadRequestPath,

    UpstreamRequestError,
    UpstreamBodyReadError,
    UpstreamStatus(StatusCode),
    UpstreamFileTooLarge{limit: u64,},
    PutFileTooLarge{limit: u64,},

    FileCreateFailed,
    FileWriteFailed,
    FileFlushFailed,
    FileSeekFailed,
    FileLockFailed,
    FileContainsNoDot,
}
impl GetRepoFileError {
    pub fn to_return(self) -> Return {
        Return {
            status: self.get_status_code(),
            content: self.get_err_content(),
            content_type: ContentType::Text,
            header_map: None,
        }
    }
    pub fn get_err_content(self) -> Content {
        match self.get_err(){
            Cow::Borrowed(v) => Content::Str(v),
            Cow::Owned(v) => Content::String(v),
        }
    }
    pub fn get_err(self) -> Cow<'static,str> {
        match self {
            Self::OpenConfig => "Error opening repo config file".into(),
            Self::ReadConfig => "Error reading repo config".into(),
            Self::ParseConfig => "Error parsing repo config".into(),
            Self::NotFound => "File or Directory could not be found".into(),
            Self::OpenFile => "Error whilst opening file".into(),
            Self::ReadDirectory => "Error whist reading directory".into(),
            Self::ReadDirectoryEntry => "Error whist reading directory entries".into(),
            Self::ReadDirectoryEntryNonUTF8Name => "Error: directory contains entries with non UTF-8 names".into(),
            Self::Panicked => "Error: implementation panicked".into(),
            Self::InvalidUTF8 => "Error: request path included invalid utf-8 characters".into(),
            Self::BadRequestPath => "Error: Request Path failed sanity checks".into(),
            Self::UpstreamRequestError => "Error: Failed to send a request to the Upstream".into(),
            Self::UpstreamBodyReadError => "Error: Failed to read the response of the Upstream".into(),
            Self::FileCreateFailed => "Error: Failed to create a file to write the upstream's response into".into(),
            Self::FileWriteFailed => "Error: Failed to write to a local file".into(),
            Self::FileFlushFailed => "Error: Failed to flush a local file".into(),
            Self::FileSeekFailed => "Error: Failed to seek a local file".into(),
            Self::FileLockFailed => "Error: Failed to lock a local file".into(),
            Self::UpstreamStatus(status) => format!("Upstream repo responded with a non 200 status code: {status}").into(),
            Self::UpstreamFileTooLarge{limit} => format!("The file is too Large. Max File size in bytes is: {limit}").into(),
            Self::PutFileTooLarge{limit} => format!("The file is too Large. Max File size in bytes is: {limit}").into(),
            Self::FileContainsNoDot => "Error: Refusing to contact upstream about files, which contain a '.' in them".into(),
        }
    }

    //noinspection RsReplaceMatchExpr - Replacement suggestion makes function non-const
    pub const fn get_status_code(self) -> Status {
        match self.allowed_status_codes_slice().first().copied() {
            Some(v) => v,
            None => Status::InternalServerError
        }
    }
    pub fn allowed_status_codes(self) -> HashSet<Status> {
        HashSet::from_iter(self.allowed_status_codes_slice().into_iter().copied())
    }
    pub const fn allowed_status_codes_slice(self) -> &'static [Status] {
        match self {
            Self::OpenConfig =>                     &[Status::InternalServerError],
            Self::ReadConfig =>                     &[Status::InternalServerError],
            Self::ParseConfig =>                    &[Status::InternalServerError],
            Self::NotFound =>                       &[Status::NotFound, Status::InternalServerError],
            Self::OpenFile =>                       &[Status::InternalServerError],
            Self::ReadDirectory =>                  &[Status::InternalServerError],
            Self::ReadDirectoryEntry =>             &[Status::InternalServerError],
            Self::ReadDirectoryEntryNonUTF8Name =>  &[Status::BadRequest, Status::InternalServerError],
            Self::Panicked =>                       &[Status::InternalServerError],
            Self::InvalidUTF8 =>                    &[Status::BadRequest, Status::InternalServerError],
            Self::BadRequestPath =>                 &[Status::BadRequest, Status::InternalServerError],
            Self::UpstreamRequestError =>           &[Status::InternalServerError],
            Self::UpstreamBodyReadError =>          &[Status::InternalServerError],
            Self::FileCreateFailed =>               &[Status::InternalServerError],
            Self::FileWriteFailed =>                &[Status::InternalServerError],
            Self::FileFlushFailed =>                &[Status::InternalServerError],
            Self::FileSeekFailed =>                 &[Status::InternalServerError],
            Self::FileLockFailed =>                 &[Status::InternalServerError],
            Self::UpstreamStatus(_) =>              &[Status::InternalServerError],
            Self::UpstreamFileTooLarge{limit: _} => &[Status::InsufficientStorage, Status::InternalServerError],
            Self::PutFileTooLarge{limit: _} =>      &[Status::PayloadTooLarge, Status::InternalServerError],
            Self::FileContainsNoDot =>              &[Status::BadRequest, Status::NotFound, Status::InternalServerError],
        }
    }
}