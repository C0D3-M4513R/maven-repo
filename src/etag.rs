use std::time::Instant;
use base64::Engine;
use crate::server_timings::AsServerTimingDuration;

pub struct ETag {
    #[allow(dead_code)]
    pub weak: bool,
    pub tag: String,
}

pub enum ETagValidator{
    Any,
    Tags(Vec<ETag>),
}

impl ETagValidator {
    pub fn parse(value: &str) -> Option<Self> {
        if value == "*" {
            return Some(Self::Any);
        }
        let mut values = Vec::new();
        for value in value.split(",") {
            let value = value.strip_prefix(" ").unwrap_or(value);
            values.push(ETag::parse(value)?);
        }
        Some(Self::Tags(values))
    }
}

impl ETag {
    pub fn parse(value: &str) -> Option<Self> {
        let weak;
        let value = match value.strip_prefix("W/") {
            Some(v) => {
                weak = true;
                v
            }
            None => {
                weak = false;
                value
            }
        };

        Some(Self{
            weak,
            tag: value.strip_prefix("\"")?.strip_suffix("\"")?.to_string(),
        })
    }
    pub async fn matches(&self, map: &memmap2::Mmap, hash: &blake3::Hash, old_hash: &tokio::sync::OnceCell<digest::Output<sha3::Sha3_512Core>>, timings: &mut Vec<String>) -> Option<bool> {
        match self.tag.strip_prefix("blake3-") {
            Some(tag) =>{
                let tag = base64::engine::general_purpose::STANDARD.decode(tag).ok()?;
                Some(tag.len() == hash.as_bytes().len() && tag == hash.as_bytes())
            }
            None => {
                let tag = base64::engine::general_purpose::STANDARD.decode(&self.tag).ok()?;
                let start = Instant::now();
                let hash = &**old_hash.get_or_init(||async{
                    use digest::Digest;
                    let mut digest = sha3::Sha3_512::default();
                    for chunk in map.chunks(4096) {
                        digest.update(chunk);
                        tokio::task::consume_budget().await;
                    }
                    digest.finalize()
                }).await;
                timings.push(format!(r#"condHeaderSha3_512;dur={};desc="Conditional Header Eval: Hashing file with sha3-512, to compare against old parsed ETag's""#, start.as_server_timing_duration()));
                Some(tag.len() == hash.len() && tag == hash)
            }
        }
    }
}