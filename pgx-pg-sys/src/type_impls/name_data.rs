use crate::NameData;

impl NameData {
    pub fn as_str(&self) -> &str {
        unsafe {
            std::str::from_utf8(std::mem::transmute(self.data.as_ref()))
                .expect("NameData is not valid UTF8")
        }
    }
}
