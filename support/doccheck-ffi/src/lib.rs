//! EditoDocCheck — wrapper C-ABI di `chk_defaced` per Edito PDF Conversion.
//!
//! `chk_defaced` (© Dario Finardi, crate pubblicato sotto AGPL-3.0, integrato
//! qui su autorizzazione dell'autore) rileva documenti "defaced": font
//! manomessi in cui il testo estratto diverge da quello disegnato (ToUnicode
//! truccati, rimappature PUA, sostituzioni semantiche di glifi) e testo
//! invisibile/occultato (vettore di prompt-injection e clausole nascoste).
//!
//! La DLL espone al processo Go la sola scansione deterministica (niente OCR,
//! niente rendering: ~35–90 ms a documento):
//!   - `EditoDocCheckScan(path_utf16) -> *mut c_char` — JSON del Report
//!     (successo) oppure JSON `{"error": "..."}` (fallimento). Mai null.
//!   - `EditoDocCheckFree(ptr)` — libera la stringa restituita.
//!
//! Il chiamante DEVE restituire ogni puntatore a `EditoDocCheckFree`: la
//! stringa è allocata da questo allocatore Rust e non va liberata altrove.

#![allow(non_snake_case)] // il nome DLL EditoDocCheck è intenzionale (deve matchare il consumer)

use std::ffi::{c_char, CString};
use std::path::PathBuf;

/// Converte il path UTF-16 null-terminated (dal lato Go/Windows) in PathBuf.
unsafe fn path_from_utf16(p: *const u16) -> Option<PathBuf> {
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(p, len);
    Some(PathBuf::from(String::from_utf16_lossy(slice)))
}

/// Confeziona una stringa per il chiamante C (interior NUL sostituiti).
fn to_c_string(s: String) -> *mut c_char {
    let cleaned = s.replace('\0', " ");
    CString::new(cleaned)
        .unwrap_or_else(|_| CString::new("{\"error\":\"encoding\"}").unwrap())
        .into_raw()
}

fn error_json(msg: &str) -> *mut c_char {
    to_c_string(
        serde_json::json!({ "error": msg.replace('"', "'") }).to_string(),
    )
}

/// Scansione deterministica anti-manomissione. Ritorna sempre una stringa
/// JSON (Report di chk_defaced oppure {"error": ...}); liberare con
/// EditoDocCheckFree.
#[no_mangle]
pub unsafe extern "system" fn EditoDocCheckScan(path: *const u16) -> *mut c_char {
    let Some(path) = path_from_utf16(path) else {
        return error_json("percorso nullo");
    };
    // La scansione parsa input arbitrari: un panic nel parser non deve
    // attraversare il confine FFI (undefined behavior) — lo intercettiamo e
    // lo riportiamo come errore JSON.
    let result = std::panic::catch_unwind(|| chk_defaced::scan::scan_path(&path, None));
    match result {
        Ok(Ok(report)) => match serde_json::to_string(&report) {
            Ok(json) => to_c_string(json),
            Err(e) => error_json(&format!("serializzazione: {e}")),
        },
        Ok(Err(e)) => error_json(&format!("{e:#}")),
        Err(_) => error_json("errore interno del parser (panic)"),
    }
}

/// Libera una stringa restituita da EditoDocCheckScan.
#[no_mangle]
pub unsafe extern "system" fn EditoDocCheckFree(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}
