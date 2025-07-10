use std::collections::HashSet;
use rocket::http::{ContentType, Status};
use crate::status::{Content, Return};

#[derive(Copy, Clone, Debug)]
pub enum GetRepoFileError{
    MainConfigError,
    OpenConfig,
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
    PutFileTooLarge,

    FileCreateFailed,
    FileWriteFailed,
    FileFlushFailed,
    FileSeekFailed,
    FileLockFailed,
    FileStartsWithDot,
}
impl GetRepoFileError {
    pub const fn to_return(self) -> Return {
        Return {
            status: self.get_status_code(),
            content: self.get_err_content(),
            content_type: ContentType::Text,
            header_map: None,
        }
    }
    pub const fn get_err_content(self) -> Content {
        Content::Str(self.get_err())
    }
    pub const fn get_err(self) -> &'static str {
        match self {
            Self::MainConfigError => "Error getting main config",
            Self::OpenConfig => "Error opening repo config file",
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
            Self::PutFileTooLarge => "The file is too Large.",
            Self::FileStartsWithDot => "Error: Refusing to contact upstream about files, which start with a '.'",
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
            Self::MainConfigError =>                &[Status::InternalServerError],
            Self::OpenConfig =>                     &[Status::InternalServerError],
            Self::ReadConfig =>                     &[Status::InternalServerError],
            Self::ParseConfig =>                    &[Status::InternalServerError],
            Self::NotFound =>                       &[Status::NotFound, Status::InternalServerError],
            Self::OpenFile =>                       &[Status::InternalServerError],
            Self::ReadDirectory =>                  &[Status::InternalServerError],
            Self::ReadDirectoryEntry =>             &[Status::InternalServerError],
            Self::ReadDirectoryEntryNonUTF8Name =>  &[Status::BadRequest, Status::InternalServerError],
            Self::ReadDirectoryEntryFileType =>     &[Status::BadRequest, Status::InternalServerError],
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
            Self::UpstreamStatus =>                 &[Status::InternalServerError],
            Self::UpstreamFileTooLarge =>           &[Status::InsufficientStorage, Status::InternalServerError],
            Self::PutFileTooLarge =>                &[Status::PayloadTooLarge, Status::InternalServerError],
            Self::FileStartsWithDot =>              &[Status::BadRequest, Status::NotFound, Status::InternalServerError],
        }
    }
}