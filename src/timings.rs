#[non_exhaustive]
pub struct ServerTimings {
    pub value: String,
}
impl ServerTimings {
    pub const fn new() -> Self {
        Self {
            value: String::new(),
        }
    }
    #[allow(dead_code)]
    pub fn push(&mut self, string: impl AsRef<str>) {
        if !self.value.is_empty() {
            self.value.push_str(", ");
        }
        self.value.push_str(string.as_ref());
    }
    pub fn push_iter_nodelim<'a, T: IntoIterator<Item = &'a str>>(&mut self, iter: T) {
        let mut added_delim = false;
        for i in iter {
            if !added_delim && !self.value.is_empty() {
                self.value.push_str(", ");
            }
            added_delim = true;
            self.value.push_str(i);
        }
    }
    pub fn append(&mut self, other: &mut Self) {
        if !self.value.is_empty() && !other.value.is_empty() {
            self.value.push_str(", ");
        }
        self.value.push_str(other.value.as_str());
    }
}