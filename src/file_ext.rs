//!Todo: This is hacky, to work around not being able to call lock on tokio's File.
//! I don't use the rust BorrowedFD -> OwnedFD, since that duplicates the file handle,
//! which might interact weirdly on non-linux platforms (it should be fine on specifically linux with the systemcalls being made).
//! This should be replaced, once there is a tokio lock call available.
trait Sealed{}
macro_rules! forward {
    (def; $name: ident, $out: ty) => {
        async fn $name (&self) -> std::io::Result<$out>;
    };
    (impl; $name: ident, $out: ty) => {
         async fn $name (&self) -> std::io::Result<$out> {
            unsafe {
                //Safety: The created file is consumed before the reference goes out of scope
                let std_file = as_file(&self).await?;
                match tokio::task::spawn_blocking(move ||{
                    let res = std_file.$name()?;
                    close_file(std_file);
                    Ok(res)
                }).await {
                    Ok(v) => v,
                    Err(err) => Err(std::io::Error::new(std::io::ErrorKind::Other, err)),
                }
            }
        }
    };
}
#[allow(private_bounds)]
pub trait FileExt: Sealed{
    fn relock(&self) -> std::io::Result<()>;
    fn relock_shared(&self) -> std::io::Result<()>;
}
impl Sealed for std::fs::File {}
impl FileExt for std::fs::File {
    fn relock(&self) -> std::io::Result<()> {
        self.unlock()?;
        self.lock()?;
        Ok(())
    }

    fn relock_shared(&self) -> std::io::Result<()> {
        self.unlock()?;
        self.lock_shared()?;
        Ok(())
    }
}
#[allow(private_bounds)]
pub trait TokioFileExt: Sealed {
    forward!(def; unlock, ());
    forward!(def; lock, ());
    forward!(def; lock_shared, ());
    forward!(def; relock, ());
    forward!(def; relock_shared, ());
}
impl Sealed for tokio::fs::File {}
impl TokioFileExt for tokio::fs::File {
    forward!(impl; unlock, ());
    forward!(impl; lock, ());
    forward!(impl; lock_shared, ());
    forward!(impl; relock, ());
    forward!(impl; relock_shared, ());
}

///Safety:
/// The input file MUST NOT be dropped before the output is dropped.
#[cfg(any(unix, target_os = "hermit", target_os = "trusty", target_os = "wasi", doc))]
async unsafe fn as_file(file: &tokio::fs::File) -> std::io::Result<core::mem::ManuallyDrop<std::fs::File>> {
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};
    //Create a std File object from the file-descriptor of the tokio File-Object.
    //This is wrapped in a ManuallyDrop, to prevent the File's drop glue from EVER closing the File-Descriptor.
    //
    //Safety: The Object is borrowed for at least the duration of this function, so the file-descriptor should also be open.
    let file = unsafe { core::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(file.as_fd().as_raw_fd())) };
    Ok(file)
}
///Safety:
/// The input file MUST NOT be dropped before the output is dropped.
#[cfg(windows)]
async unsafe fn as_file(file: &tokio::fs::File) -> std::io::Result<core::mem::ManuallyDrop<std::fs::File>> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    //Create a std File object from the file-descriptor of the tokio File-Object.
    //This is wrapped in a ManuallyDrop, to prevent the File's drop glue from EVER closing the File-Descriptor.
    //
    //Safety: The Object is borrowed for at least the duration of this function, so the file-descriptor should also be open.
    let file = unsafe { core::mem::ManuallyDrop::new(std::fs::File::from_raw_handle(file.as_raw_handle())) };
    Ok(file)
}

///Safety:
/// The input file MUST NOT be dropped before the output is dropped.
#[cfg(not(any(unix, target_os = "hermit", target_os = "trusty", target_os = "wasi", doc, windows)))]
async unsafe fn as_file(file: &tokio::fs::File) -> std::io::Result<core::mem::ManuallyDrop<std::fs::File>> {
    Ok(core::mem::ManuallyDrop::new(file.try_clone().await?.into_std().await))
}

/// Safety: the file-handle must have been produced by the as_file function in this file.
#[cfg(any(unix, target_os = "hermit", target_os = "trusty", target_os = "wasi", windows, doc))]
unsafe fn close_file(#[allow(unused_variables)] file: core::mem::ManuallyDrop<std::fs::File>){
}
/// Safety: the file-handle must have been produced by the as_file function in this file.
#[cfg(not(any(unix, target_os = "hermit", target_os = "trusty", target_os = "wasi", doc, windows)))]
unsafe fn close_file(file: core::mem::ManuallyDrop<std::fs::File>){
    //This is actually a new file-descriptor, so we have to close it?
    drop(core::mem::ManuallyDrop::into_inner(file))
}
