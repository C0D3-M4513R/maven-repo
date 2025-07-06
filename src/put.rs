use std::borrow::Cow;
use std::io::{Cursor, Error, ErrorKind};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use digest::Digest;
use rocket::data::{ByteUnit, ToByteUnit};
use rocket::http::{ContentType, Status};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;
use crate::auth::BasicAuthentication;
use crate::err::GetRepoFileError;
use crate::path_info::PathInfo;
use crate::repository::get_repo_config;
use crate::status::{Content, Return};

#[rocket::put("/<repo>/<path..>", data="<data>")]
pub async fn put_repo_file(repo: &str, path: PathBuf, auth: Option<Result<BasicAuthentication, Return>>, data: rocket::data::Data<'_>) -> Return {
    let auth = match auth {
        Some(Err(err)) => return err,
        Some(Ok(v)) => Some(v),
        None => None,
    };
    if path.components().any(|v|
        match v {
            Component::ParentDir => true,
            Component::RootDir => true,
            Component::Prefix(_) => true,
            _ => false,
        }
    ) {
        return GetRepoFileError::BadRequestPath.to_return();
    }
    if path.has_root() {
        return GetRepoFileError::BadRequestPath.to_return();
    }
    let str_path = match path.to_str() {
        None => return GetRepoFileError::InvalidUTF8.to_return(),
        Some(v) => v,
    };
    let str_path = str_path.strip_prefix("/").unwrap_or(str_path);
    let str_path = str_path.strip_suffix("/").unwrap_or(str_path);

    let config = match get_repo_config(Cow::Borrowed(repo)).await {
        Ok(v) => v,
        Err(e) => return e.to_return(),
    };


    if !config.upstreams.is_empty() {
        return Return {
            status: Status::Forbidden,
            content: Content::Str("It's forbidden to deploy to a repo, which has remotes."),
            content_type: ContentType::Text,
            header_map: None,
        }
    }
    
    match config.check_auth(rocket::http::Method::Put, auth, str_path) {
        Err(err) => return err,
        Ok(_) => {},
    }
    
    let info = match PathInfo::parse(path.as_path()) {
        Ok(v) => v,
        Err(err) => return err,
    };
    let metadata = match info.get_merged_metadata(repo, rocket::http::Method::Put).await {
        Ok(v) => v,
        Err(err) => return err,
    };

    match create_file_dirs(repo, &path).await {
        Ok(()) => {},
        Err(err) => return err,
    }
    let file = match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .await
    {
        Ok(v) => v,
        Err(err) => {
            tracing::error!("Failed to create new file dirs while deploying {}: {err}", path.display());
            return match err.kind() {
                ErrorKind::AlreadyExists => Return {
                    status: Status::Conflict,
                    content: Content::Str("File already exists"),
                    content_type: ContentType::Text,
                    header_map: None,
                },
                _ => Return {
                    status: Status::InternalServerError,
                    content: Content::Str("Failed creating file"),
                    content_type: ContentType::Text,
                    header_map: None,
                }
            }
        }
    };
    let max_file_size = config.max_file_size.unwrap_or(crate::DEFAULT_MAX_FILE_SIZE);
    match put_file(file, path.clone(), max_file_size, data.open(ByteUnit::max_value())).await {
        Ok(_) => {},
        Err(err) => return err,
    };
    let mut js = JoinSet::new();
    for (path, file, _, ser) in metadata {
        js.spawn(put_file(file, path, max_file_size, Cursor::new(ser)));
    }
    while let Some(task) = js.join_next().await {
        match task {
            Ok(Ok(_)) => {},
            Ok(Err(err)) => return err,
            Err(err) => {
                tracing::error!("Panicked whilst updating maven-metadata for deployment of {}: {err}", path.display());
                js.abort_all();
                return Return {
                    status: Status::InternalServerError,
                    content: Content::Str("Panicked whilst updating maven-metadata"),
                    content_type: ContentType::Text,
                    header_map: None,
                };
            }
        }
    }

    Return{
        status: Status::Created,
        content: Content::Str(""),
        content_type: ContentType::Text,
        header_map: None,
    }
}

async fn create_file_dirs(repo: &str, path: &Path) -> Result<(), Return> {
    let file_path = Path::new(repo).join(&path);
    let parent = match file_path.parent() {
        Some(v) => v,
        None => return Err(Return {
            status: Status::BadRequest,
            content: Content::Str("Deploy path has no proper parent directory"),
            content_type: ContentType::Text,
            header_map: None,
        }),
    };
    match tokio::fs::create_dir_all(parent).await {
        Ok(()) => Ok(()),
        Err(err) => {
            tracing::error!("Failed to create dirs while deploying {}: {err}", path.display());
            Err(Return {
                status: Status::InternalServerError,
                content: Content::Str("Failed to create parent directories."),
                content_type: ContentType::Text,
                header_map: None,
            })
        }
    }
}
async fn put_file<D: tokio::io::AsyncRead + Unpin>(file: File, file_path: PathBuf, limit: u64, mut data: D) -> Result<Vec<PathBuf>, Return> {
    let mut files = Vec::with_capacity(1 + 4);
    let mut file = WriteFile {
        file: tokio::io::BufWriter::new(file),
        limit,
        read: 0,
        hashers: Default::default(),
    };
    files.push(file_path.clone());
    //Write to file
    match tokio::io::copy(&mut data, &mut file).await {
        Ok(_) => {},
        Err(err) => {
            tracing::error!("Failed to write to file {}: {err}", file_path.display());
            remove_files(&files).await;
            return Err(match err.kind() {
                ErrorKind::FileTooLarge => GetRepoFileError::PutFileTooLarge.to_return(),
                _ => GetRepoFileError::FileWriteFailed.to_return(),
            });
        }
    }
    match file.shutdown().await {
        Ok(()) => {},
        Err(err) => {
            tracing::error!("Failed to finalize write to {}: {err}", file_path.display());
            remove_files(&files).await;
            return Err(Return {
                status: Status::InternalServerError,
                content: Content::Str("Failed to finish writing to file"),
                content_type: ContentType::Text,
                header_map: None,
            })
        }
    }

    macro_rules! write_file_hash {
        ($hasher:ident, $extension: literal) => {
            let hasher = $hasher;
            let hash_file_path = match file_path.extension() {
                Some(v) => {
                    let mut v = v.to_os_string();
                    v.push(".");
                    v.push($extension);
                    file_path.with_extension(v.as_os_str())
                },
                None => {
                    file_path.with_extension($extension)
                }
            }; 
            let mut file = match tokio::fs::File::create_new(&hash_file_path).await {
                Ok(v) => v,
                Err(err) => {
                    tracing::error!("Failed to create hash of file {}.{}: {err}", file_path.display(), $extension);
                    remove_files(&files).await;
                    return Err(Return{
                        status: Status::InternalServerError,
                        content: Content::Str("Failed to create file for storing the File hash"),
                        content_type: ContentType::Text,
                        header_map: None,
                    })
                }
            };
            files.push(hash_file_path);
            let hash = hasher.finalize();
            let hash = data_encoding::HEXLOWER.encode(hash.as_slice());
            match file.write_all(hash.as_bytes()).await {
                Ok(()) => {},
                Err(err) => {
                    tracing::error!("Failed to write hash of file {}.{}: {err}", file_path.display(), $extension);
                    remove_files(&files).await;
                    return Err(Return{
                        status: Status::InternalServerError,
                        content: Content::Str("Failed to write file hash"),
                        content_type: ContentType::Text,
                        header_map: None,
                    })
                }
            }
            match file.shutdown().await {
                Ok(()) => {},
                Err(err) => {
                    tracing::error!("Failed to finalize write hash of file {}.{}: {err}", file_path.display(), $extension);
                    remove_files(&files).await;
                    return Err(Return{
                        status: Status::InternalServerError,
                        content: Content::Str("Failed to finalize write file hash"),
                        content_type: ContentType::Text,
                        header_map: None,
                    })
                }
            }
        };
    }
    let (md5, sha1, sha2_256, sha2_512) = file.hashers;
    write_file_hash!(md5, "md5");
    write_file_hash!(sha1, "sha1");
    write_file_hash!(sha2_256, "sha256");
    write_file_hash!(sha2_512, "sha512");
    let file = file.file.into_inner().into_std().await;
    match tokio::task::spawn_blocking(move ||{
        file.unlock()
    }).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::error!("Error whilst unlocking file {}: {err}", file_path.display());
            return Err(Return {
                status: Status::InternalServerError,
                content: Content::Str("Error whilst unlocking file"),
                content_type: ContentType::Text,
                header_map: None,
            })
        }
        Err(err) => {
            tracing::error!("Panicked whilst unlocking file {}: {err}", file_path.display());
            return Err(Return {
                status: Status::InternalServerError,
                content: Content::Str("Panicked whilst unlocking file"),
                content_type: ContentType::Text,
                header_map: None,
            })
        }
    }

    Ok(files)
}
async fn remove_files(files: &Vec<PathBuf>) {
    for file in files {
        match tokio::fs::remove_file(&file).await {
            Ok(()) => {},
            Err(err) => {
                tracing::error!("Error deleting File after error writing to File {}: {err}", file.display());
            }
        }
    }
}

struct WriteFile {
    file: tokio::io::BufWriter<tokio::fs::File>,
    limit: u64,
    read: u64,
    hashers: (md5::Md5, sha1_checked::Sha1, sha2::Sha256, sha2::Sha512),
}
impl tokio::io::AsyncWrite for WriteFile {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
        if self.read >= self.limit {
            return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::FileTooLarge, anyhow::anyhow!("Configured File Limit reached"))));
        }
        let written = match Pin::new(&mut self.file).poll_write(cx, buf) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
            Poll::Ready(Ok(ok)) => ok,
        };
        let buf = &buf[..written];

        use digest::Digest;
        let (md5, sha1, sha2_256, sha2_512) = &mut self.hashers;
        md5.update(buf);
        sha1.update(buf);
        sha2_256.update(buf);
        sha2_512.update(buf);
        Poll::Ready(Ok(written))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.file).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.file).poll_shutdown(cx)
    }
}