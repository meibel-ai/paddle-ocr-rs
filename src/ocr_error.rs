use thiserror::Error;

#[derive(Error, Debug)]
pub enum OcrError {
    #[error("Ort error")]
    Ort(#[from] ort::Error),
    #[error("Io error")]
    Io(#[from] std::io::Error),
    #[error("Session not initialized")]
    ImageError(#[from] image::ImageError),
    #[error("Image error")]
    SessionNotInitialized,
    /// Errore nella shape o nei nomi degli input ONNX (modello incompatibile).
    #[error("Model input error: {0}")]
    ModelInput(String),
    /// Errore nella shape o decodifica dell'output ONNX.
    #[error("Model output error: {0}")]
    ModelOutput(String),
    /// Errore download / cache modelli (model_hub).
    #[error("Model hub error: {0}")]
    ModelHubError(String),
}

// [ort rc.13] `ort::Error` e' diventato generico su un parametro di *recupero*:
// `SessionBuilder::commit_*` restituisce `Error<SessionBuilder>` (che consente di riprendere il
// builder dopo il fallimento) invece del piatto `Error`. `ort` fornisce la conversione verso
// `Error<()>`; qui la si aggancia a `OcrError` cosi' il `?` continua a funzionare nei chiamanti.
impl From<ort::Error<ort::session::builder::SessionBuilder>> for OcrError {
    fn from(e: ort::Error<ort::session::builder::SessionBuilder>) -> Self {
        OcrError::Ort(ort::Error::from(e))
    }
}
