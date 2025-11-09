use crate::file::File;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, DocumentError>;

#[derive(Error, Debug)]
pub enum DocumentError {
    #[error("IO error")]
    Io(#[from] std::io::Error),
}

/// Abstract source of UTF-8 text (no direct fs:: usage required).
pub struct Document {
    //name: String,
    //base: String,
    //ext: String,
    file: File,
}

impl Document {
    pub fn new(_name: String, _base: String, _ext: String, file: File) -> Self {
        Self {
            //name,
            //base,
            //ext,
            file,
        }
    }

    pub fn read_to_string(&self) -> Result<String> {
        Ok(self.file.read_string()?)
    }
}
