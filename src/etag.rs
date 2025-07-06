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
}