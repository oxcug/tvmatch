use thiserror::Error;

#[derive(Debug, Error)]
pub enum IsobmffError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed box: {0}")]
    Malformed(String),

    #[error("unsupported box variant: {0}")]
    Unsupported(String),

    #[error("required box missing: {0}")]
    MissingBox(&'static str),

    #[error("heif: {0}")]
    Heif(String),
}

pub type IsobmffResult<T> = Result<T, IsobmffError>;
