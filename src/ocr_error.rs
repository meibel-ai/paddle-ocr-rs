use thiserror::Error;

#[derive(Error, Debug)]
pub enum OcrError {
    #[error("Ort error: {0}")]
    Ort(#[from] ort::Error),
    #[error("Io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image error: {0}")]
    ImageError(#[from] image::ImageError),
    #[error("Session not initialized")]
    SessionNotInitialized,
    /// A model returned a shape the caller cannot interpret — e.g. a
    /// recognition batch of N crops coming back with a different N.
    #[error("Model output mismatch: {0}")]
    OutputMismatch(String),
}
