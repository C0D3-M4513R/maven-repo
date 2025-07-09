use base64::Engine;

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
    pub async fn matches(&self, hash: &blake3::Hash) -> Option<bool> {
        match self.tag.strip_prefix("blake3-") {
            Some(tag) =>{
                let tag = base64::engine::general_purpose::STANDARD.decode(tag).ok()?;
                Some(tag.len() == hash.as_bytes().len() && tag == hash.as_bytes())
            }
            None => {
                Some(false)
            }
        }
    }
}