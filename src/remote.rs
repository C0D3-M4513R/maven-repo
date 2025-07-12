use std::net::IpAddr;
use std::ops::Deref;
use std::time::Duration;
use reqwest::StatusCode;
use crate::repository::RemoteUpstream;

pub fn get_remote_request(
    remote: &RemoteUpstream,
    str_path: &str,
    request_url: &str,
    remote_client: Option<IpAddr>,
) -> (String, reqwest::RequestBuilder) {
    let mut url = remote.url.clone();
    match str_path.strip_prefix("/") {
        Some(v) => {
            if url.ends_with("/") {
                url.push_str(v);
            }else {
                url.push_str(str_path);
            }
        }
        None => {
            if !url.ends_with("/") {
                url.push('/');
            }
            url.push_str(str_path);
        }
    }
    let url = url;
    let mut req = crate::CLIENT
        .get(&url)
        .timeout(remote.timeout);

    if !request_url.is_empty() {
        req = req.header("X-Downstream-Repo-Link", request_url);
    }

    if let Some(v) = remote_client {
        req = req.header("X-Forwarded-For", v.to_canonical().to_string());
    }

    (url, req)
}
pub async fn read_remote<T: Deref<Target = str>>(url: T, timeout: Duration, headers: reqwest::header::HeaderMap) -> anyhow::Result<(T, reqwest::Response, Vec<u8>, blake3::Hash)>{
    let mut res = crate::CLIENT.get(&*url)
        .timeout(timeout)
        .headers(headers)
        .send()
        .await?;
    let is304 = res.status() == StatusCode::NOT_MODIFIED;
    if res.status() != StatusCode::OK && !is304 {
        return Err(anyhow::Error::msg(format!("Response Status-Code was not Ok or NotModified: {res:?}")));
    }
    let mut bytes = if is304 {Vec::new()} else {Vec::with_capacity(res.content_length().unwrap_or(0) as usize)};
    let mut hash = blake3::Hasher::new();
    if !is304 {
        while let Some(chunk) = res.chunk().await? {
            bytes.extend_from_slice(chunk.as_ref());
            hash.update(chunk.as_ref());
        }
    }
    Ok((url, res, bytes, hash.finalize()))
}