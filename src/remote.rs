use std::net::IpAddr;
use std::time::Duration;
use reqwest::StatusCode;
use crate::repository::RemoteUpstream;

pub fn get_remote_request(
    remote: &RemoteUpstream,
    str_path: &str,
    request_url: &str,
    remote_client: Option<IpAddr>,
) -> (String, reqwest::RequestBuilder) {
    let url = &remote.url;
    let url = format!("{url}/{str_path}");
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
pub async fn read_remote(url: String, timeout: Duration, headers: reqwest::header::HeaderMap) -> anyhow::Result<(String, reqwest::Response, Vec<u8>, blake3::Hash)>{
    let mut res = crate::CLIENT.get(url.as_str())
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