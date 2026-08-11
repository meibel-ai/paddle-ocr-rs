//! Compat shim per l'estrazione dei tensori: isola in **un solo punto** la differenza di firma
//! di `Value::try_extract_tensor` fra le release candidate di `ort`.
//!
//! ## Storia della firma
//!
//! - **rc.9**: `Result<ArrayViewD<T>, ort::Error>` — shape e dati si ricavano dalla view.
//! - **rc.11+ (incluso rc.13, qui pinnato)**: `Result<(&Shape, &[T])>` — tupla con shape e slice.
//!
//! Il crate e' nato su rc.9 e questo modulo adattava rc.11→rc.9; con l'aggiornamento a **rc.13**
//! il verso si e' invertito: adatta la tupla di rc.13 alle stesse due funzioni di prima. I circa
//! otto punti d'uso non sono stati toccati — ed e' esattamente il motivo per cui lo shim esiste:
//! un cambio di firma a monte si assorbe qui, non sparso nel codice.

use crate::ocr_error::OcrError;
use ort::value::Value;

/// Estrae un tensore f32 come `Vec<f32>`, scartando la shape.
pub fn tensor_to_vec_f32(value: &Value) -> Result<Vec<f32>, OcrError> {
    let (_shape, data) = value.try_extract_tensor::<f32>()?;
    Ok(data.to_vec())
}

/// Estrae un tensore f32 con la shape (per output di shape variabile).
/// Ritorna `(shape, data)` con la shape gia' copiata in `Vec<i64>`.
pub fn tensor_extract_with_shape_f32(value: &Value) -> Result<(Vec<i64>, Vec<f32>), OcrError> {
    let (shape, data) = value.try_extract_tensor::<f32>()?;
    let shape: Vec<i64> = shape.iter().map(|&d| d as i64).collect();
    Ok((shape, data.to_vec()))
}
