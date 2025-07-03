use std::borrow::Cow;
use std::collections::HashSet;
use reqwest::StatusCode;
use rocket::http::Status;
use crate::status::Content;

#[derive(Copy, Clone, Debug)]
pub enum GetRepoFileError{
    OpenConfig,
    ReadConfig,
    ParseConfig,

    NotFound,

    ReadFile,
    ReadDirectory,
    ReadDirectoryEntry,
    ReadDirectoryEntryNonUTF8Name,

    Panicked,

    InvalidUTF8,

    UpstreamRequestError,
    UpstreamBodyReadError,
    UpstreamStatus(StatusCode),
    UpstreamFileTooLarge{limit: u64,},
    PutFileTooLarge{limit: u64,},

    FileCreateFailed,
    FileWriteFailed,
    FileFlushFailed,
    FileContainsNoDot,

    Unauthorized,
    Forbidden,
}
impl GetRepoFileError {
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
            Self::ReadFile => "Error whilst reading file".into(),
            Self::ReadDirectory => "Error whist reading directory".into(),
            Self::ReadDirectoryEntry => "Error whist reading directory entries".into(),
            Self::ReadDirectoryEntryNonUTF8Name => "Error: directory contains entries with non UTF-8 names".into(),
            Self::Panicked => "Error: implementation panicked".into(),
            Self::InvalidUTF8 => "Error: request path included invalid utf-8 characters".into(),
            Self::UpstreamRequestError => "Error: Failed to send a request to the Upstream".into(),
            Self::UpstreamBodyReadError => "Error: Failed to read the response of the Upstream".into(),
            Self::FileCreateFailed => "Error: Failed to create a file to write the upstream's response into".into(),
            Self::FileWriteFailed => "Error: Failed to write to a local file to contain the upstream's response".into(),
            Self::FileFlushFailed => "Error: Failed to flush a local file".into(),
            Self::UpstreamStatus(status) => format!("Upstream repo responded with a non 200 status code: {status}").into(),
            Self::UpstreamFileTooLarge{limit} => format!("The file is too Large. Max File size in bytes is: {limit}").into(),
            Self::PutFileTooLarge{limit} => format!("The file is too Large. Max File size in bytes is: {limit}").into(),
            Self::FileContainsNoDot => "Error: Refusing to contact upstream about files, which contain a '.' in them".into(),
            Self::Unauthorized => "Unauthorized".into(),
            Self::Forbidden => "Forbidden".into(),
        }
    }
    
    pub fn get_status_code(self) -> Status {
        self.allowed_status_codes().drain().next().unwrap_or(Status::InternalServerError)
    }
    pub fn allowed_status_codes(self) -> HashSet<Status> {
        match self {
            Self::OpenConfig => HashSet::from([Status::InternalServerError]),
            Self::ReadConfig => HashSet::from([Status::InternalServerError]),
            Self::ParseConfig => HashSet::from([Status::InternalServerError]),
            Self::NotFound => HashSet::from([Status::NotFound, Status::InternalServerError]),
            Self::ReadFile => HashSet::from([Status::InternalServerError]),
            Self::ReadDirectory => HashSet::from([Status::InternalServerError]),
            Self::ReadDirectoryEntry => HashSet::from([Status::InternalServerError]),
            Self::ReadDirectoryEntryNonUTF8Name => HashSet::from([Status::BadRequest, Status::InternalServerError]),
            Self::Panicked => HashSet::from([Status::InternalServerError]),
            Self::InvalidUTF8 => HashSet::from([Status::BadRequest, Status::InternalServerError]),
            Self::UpstreamRequestError => HashSet::from([Status::InternalServerError]),
            Self::UpstreamBodyReadError => HashSet::from([Status::InternalServerError]),
            Self::FileCreateFailed => HashSet::from([Status::InternalServerError]),
            Self::FileWriteFailed => HashSet::from([Status::InternalServerError]),
            Self::FileFlushFailed => HashSet::from([Status::InternalServerError]),
            Self::UpstreamStatus(_) => HashSet::from([Status::InternalServerError]),
            Self::UpstreamFileTooLarge{limit: _} => HashSet::from([Status::InsufficientStorage, Status::InternalServerError]),
            Self::PutFileTooLarge{limit: _} => HashSet::from([Status::PayloadTooLarge, Status::InternalServerError]),
            Self::FileContainsNoDot => HashSet::from([Status::BadRequest, Status::NotFound, Status::InternalServerError]),
            Self::Unauthorized => HashSet::from([Status::NotFound, Status::InternalServerError]),
            Self::Forbidden => HashSet::from([Status::Forbidden, Status::NotFound, Status::InternalServerError]),
        }
    }
}