//! In-process render of `docx_to_html` via a Tauri webview (`wry`/`tao`) — feature `render-wry`.
//!
//! Why a webview and not a PDF converter: a browser engine (WebView2 / WKWebView / WebKitGTK) applies the
//! document's embedded `@font-face` cmaps **faithfully**, drawing the glyphs the (possibly tampered) font
//! actually carries. DOCX→PDF converters that re-shape glyphs (Skia, Typst) launder the attack back to
//! clean text. Rendering here + OCR on the result is the only way to recover the **localized/positional**
//! A3 defacement that the deterministic outline detectors structurally cannot represent.
//!
//! Transport: the HTML is handed straight to the webview with [`wry::WebViewBuilder::with_html`] — there
//! is **no HTTP server and no TCP port** (the constraint that a fixed port could already be taken simply
//! does not arise). The rendered window is captured to a PNG with `xcap`. The page signals readiness over
//! wry's IPC bridge once `document.fonts.ready` resolves, so we capture only after the embedded fonts are
//! applied — not a blind timed guess.

use std::borrow::Cow;
use std::path::Path;

use anyhow::{bail, Context, Result};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// Capture parameters for [`render_html_to_png`].
pub struct RenderOptions {
    /// Webview viewport width in logical pixels.
    pub width: u32,
    /// Webview viewport height in logical pixels (tall enough to hold the content to OCR).
    pub height: u32,
    /// Extra settle time (ms) after `fonts.ready` before the capture, covering async glyph paint.
    pub settle_ms: u64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { width: 1100, height: 1400, settle_ms: 250 }
    }
}

#[derive(Debug)]
enum UserEvent {
    /// The page reported `document.fonts.ready` (embedded fonts applied) and its full content height
    /// (CSS px) over the IPC bridge — so we can grow the off-screen viewport to capture the whole doc.
    Ready(u32),
}

/// Upper bound on the captured viewport height (CSS px). WebView2/compositor surfaces have practical
/// size limits; a very long document is truncated here rather than failing the capture.
const MAX_HEIGHT: u32 = 20_000;

/// A unique window title so `xcap` can find exactly this window among all open ones.
const WINDOW_TITLE: &str = "chk_defaced-render-surface";

/// Render `html` in an in-process webview and write the captured window to `out` (PNG).
///
/// Loads the HTML directly (no server/port), waits for the embedded fonts to apply, captures the window
/// with `xcap`, and tears the webview down. Returns an error if the page never signals readiness or the
/// window cannot be captured.
pub fn render_html_to_png(html: &str, out: &Path, opts: &RenderOptions) -> Result<()> {
    // Append a readiness probe: once the embedded fonts have loaded and two frames have painted, report
    // readiness AND the document's full content height over wry's IPC bridge (`window.ipc.postMessage`),
    // so the host can grow the viewport to capture the whole page (WebView2 only paints what's visible).
    let ready_script = "<script>document.fonts.ready.then(function(){\
        requestAnimationFrame(function(){requestAnimationFrame(function(){\
        var h=Math.ceil(document.documentElement.scrollHeight);\
        if(window.ipc&&window.ipc.postMessage){window.ipc.postMessage('ready:'+h);}});});});</script>";
    let page = inject_before_body_end(html, ready_script);

    // Allow the event loop to run off the main thread (Windows/Linux): the CLI calls this from its scan
    // loop and the server from worker threads, neither guaranteed to be the main thread. macOS still
    // requires the main thread (UI restriction) — the caller must arrange that there.
    let mut builder = EventLoopBuilder::<UserEvent>::with_user_event();
    #[cfg(windows)]
    {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        builder.with_any_thread(true);
    }
    #[cfg(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))]
    {
        use tao::platform::unix::EventLoopBuilderExtUnix;
        builder.with_any_thread(true);
    }
    let mut event_loop = builder.build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title(WINDOW_TITLE)
        .with_inner_size(tao::dpi::LogicalSize::new(opts.width as f64, opts.height as f64))
        .with_position(tao::dpi::LogicalPosition::new(-4000.0, -4000.0)) // off-screen: no visible flash
        .with_visible(true) // still mapped/painted so PrintWindow has content to capture
        .build(&event_loop)
        .context("creating render window")?;

    // Serve the page over an in-memory custom protocol rather than `with_html`: WebView2's
    // `NavigateToString` caps the string at ~2 MB, and our page (megabytes of base64-embedded fonts)
    // blows past it. The custom scheme keeps everything in process — no temp file, no server, no port.
    let body = page.into_bytes();
    let _webview = WebViewBuilder::new(&window)
        .with_custom_protocol("chkd".into(), move |_req| {
            wry::http::Response::builder()
                .header("Content-Type", "text/html")
                .body(Cow::<'static, [u8]>::Owned(body.clone()))
                .unwrap()
        })
        .with_url("chkd://render/index.html")
        .with_background_color((255, 255, 255, 255))
        .with_ipc_handler(move |req| {
            if let Some(h) = req.body().strip_prefix("ready:").and_then(|s| s.parse::<u32>().ok()) {
                let _ = proxy.send_event(UserEvent::Ready(h));
            }
        })
        .build()
        .context("creating webview")?;

    let mut captured: Option<Result<()>> = None;
    let settle = std::time::Duration::from_millis(opts.settle_ms);
    // Hard ceiling so a page that never fires `ready` cannot hang the tool.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    // After we grow the viewport to the content height, WebView2 must repaint the newly revealed area
    // before we capture — we keep pumping the loop until this instant (a blocking sleep would freeze the
    // repaint). `None` until the page reports its height.
    let mut capture_at: Option<std::time::Instant> = None;

    event_loop.run_return(|event, _, control_flow| {
        match event {
            Event::UserEvent(UserEvent::Ready(content_h)) if capture_at.is_none() => {
                // Grow the off-screen viewport to the whole document so the capture isn't clipped.
                let h = content_h.clamp(opts.height, MAX_HEIGHT);
                window.set_inner_size(tao::dpi::LogicalSize::new(opts.width as f64, h as f64));
                capture_at = Some(std::time::Instant::now() + settle + std::time::Duration::from_millis(400));
            }
            _ => {}
        }
        match capture_at {
            Some(at) if std::time::Instant::now() >= at => {
                captured = Some(capture_window(&window, out));
                *control_flow = ControlFlow::Exit;
            }
            Some(at) => *control_flow = ControlFlow::WaitUntil(at),
            None if std::time::Instant::now() > deadline => {
                captured = Some(Err(anyhow::anyhow!("render timed out before fonts.ready")));
                *control_flow = ControlFlow::Exit;
            }
            None => {
                *control_flow =
                    ControlFlow::WaitUntil(std::time::Instant::now() + std::time::Duration::from_millis(100));
            }
        }
    });

    match captured {
        Some(r) => r,
        None => bail!("render loop exited without capturing"),
    }
}

/// Capture the render window's client area to `out` (PNG). On Windows we capture by HWND with
/// `PrintWindow(PW_RENDERFULLCONTENT)` (off-screen/background-safe); elsewhere we fall back to `xcap`
/// matching the unique window title.
#[cfg(windows)]
fn capture_window(window: &tao::window::Window, out: &Path) -> Result<()> {
    use core::ffi::c_void;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
        ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

    let raw = window.window_handle().context("acquiring window handle")?.as_raw();
    let RawWindowHandle::Win32(h) = raw else { bail!("render window is not a Win32 window") };
    let hwnd = HWND(h.hwnd.get() as *mut c_void);

    unsafe {
        let mut rect = RECT::default();
        GetClientRect(hwnd, &mut rect).context("GetClientRect")?;
        let w = (rect.right - rect.left).max(1);
        let h = (rect.bottom - rect.top).max(1);

        let hdc_win = GetDC(hwnd);
        let hdc_mem = CreateCompatibleDC(hdc_win);
        let hbmp = CreateCompatibleBitmap(hdc_win, w, h);
        let prev = SelectObject(hdc_mem, HGDIOBJ(hbmp.0));

        // PW_CLIENTONLY (0x1) — skip the title bar; PW_RENDERFULLCONTENT (0x2) — capture the
        // DirectComposition/WebView2 surface rather than a blank GPU window.
        let ok = PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(0x1 | 0x2)).as_bool();

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative → top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        let scanlines =
            GetDIBits(hdc_mem, hbmp, 0, h as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);

        SelectObject(hdc_mem, prev);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_win);

        if !ok || scanlines == 0 {
            bail!("PrintWindow/GetDIBits captured no pixels");
        }
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA → RGBA
            px[3] = 255; // window DC has no real alpha
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, buf)
            .context("building image from captured pixels")?;
        img.save(out).with_context(|| format!("writing {}", out.display()))?;
    }
    Ok(())
}

/// Non-Windows capture: match the unique window title via `xcap`.
#[cfg(not(windows))]
fn capture_window(_window: &tao::window::Window, out: &Path) -> Result<()> {
    let windows = xcap::Window::all().context("enumerating windows for capture")?;
    let target = windows.iter().find(|w| w.title() == WINDOW_TITLE).cloned();
    let Some(target) = target else {
        let titles: Vec<String> =
            windows.iter().map(|w| w.title().to_string()).filter(|t| !t.is_empty()).collect();
        bail!("render window not found for capture; visible window titles seen: {titles:?}");
    };
    let image = target.capture_image().context("capturing render window")?;
    image.save(out).with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

/// **Render-OCR confirmation for DOCX** (features `render-wry` + `ocr-tesseract`) — the doubt-resolving
/// fallback. Renders the document with its embedded (possibly tampered) fonts, OCRs the result (what a
/// human sees), and compares with the extracted `w:t` body text (what a machine reads). Mirrors the PDF
/// [`crate::atlas::verify_with_render`]:
/// - a real word substitution rendered↔extracted → **Confirmed** (+ `DOCX.RENDER_DIVERGENCE_CONFIRMED`);
/// - none, and the render closely matches the extract → **Refuted** (a glyph collision is present but not
///   used on the visible text → the `GLYPH_SEMANTIC_REPLACEMENT` findings drop to `Info`);
/// - otherwise (inconclusive OCR) → left **Unconfirmed**, so the deterministic findings stand.
///
/// It also fills each phrase's `ocr` ground truth from the rendered text. This is the **only** way to
/// confirm the localized/positional A3 attack on DOCX (per-letter font subsets) — converters that
/// re-shape glyphs launder it; a real webview render does not.
#[cfg(feature = "ocr-tesseract")]
pub fn verify_docx_with_render(
    report: &mut crate::finding::Report,
    docx: &Path,
    ocr: &dyn crate::ocr::OcrEngine,
) -> Result<()> {
    use crate::finding::{Category, Finding, Severity, Verdict};
    use crate::textdiff::{jaccard, missing_words, significant_substitutions};

    let has_collision = report.findings.iter().any(|f| f.rule.contains("GLYPH_SEMANTIC_REPLACEMENT"));
    let has_hidden_candidate = report.findings.iter().any(|f| {
        matches!(f.category, Category::HiddenContent)
            && (f.rule.contains("INVISIBLE") || f.rule.contains("TINY"))
    });
    if !has_collision && report.phrases.is_empty() && !has_hidden_candidate {
        return Ok(()); // nothing in doubt → no need to render
    }

    // 1. Render the DOCX (embedded fonts applied) to a temporary PNG, then 2. OCR it.
    let html = crate::docx_html::docx_to_html(docx)?;
    let tmp = render_temp_path();
    render_html_to_png(&html, &tmp, &RenderOptions::default())?;
    let visual = match ocr_png(&tmp, ocr) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let _ = std::fs::remove_file(&tmp);

    // 3. Extracted body text (same scope as the render: document.xml only).
    let extracted = crate::docx_html::docx_body_text(docx)?;

    // Hidden-text confirmation: words in the extract absent from the OCR of the render are invisible.
    if has_hidden_candidate {
        let hidden = missing_words(&extracted, &visual);
        if hidden.len() >= 4 {
            let list = hidden.iter().take(20).cloned().collect::<Vec<_>>().join(" ");
            report.push(Finding::new(
                "DOCX.HIDDEN_TEXT_CONFIRMED",
                Severity::High,
                Category::HiddenContent,
                "rendered document",
                format!("render-OCR confirms {} word(s) in the extract but not visible in the render: {list}", hidden.len()),
                0.85,
            ));
        }
    }

    // (b) phrase OCR ground truth — attach the closest rendered sentence to each affected phrase.
    let ocr_sents = crate::finding::split_sentences(&visual);
    for ph in &mut report.phrases {
        let best = ocr_sents.iter().max_by(|a, b| {
            let sa = jaccard(&ph.presumed, a).max(jaccard(&ph.extracted, a));
            let sb = jaccard(&ph.presumed, b).max(jaccard(&ph.extracted, b));
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(s) = best {
            if jaccard(&ph.presumed, s).max(jaccard(&ph.extracted, s)) >= 0.4 {
                ph.ocr = Some(s.clone());
            }
        }
    }

    // (a) verdict on the collision signal.
    if !has_collision {
        return Ok(());
    }
    let subs = significant_substitutions(&extracted, &visual);
    let sim = jaccard(&extracted, &visual);
    if !subs.is_empty() {
        report.verdict = Some(Verdict::Confirmed);
        let list: String =
            subs.iter().take(10).map(|(e, v)| format!("'{e}'→'{v}'")).collect::<Vec<_>>().join(", ");
        report.push(Finding::new(
            "DOCX.RENDER_DIVERGENCE_CONFIRMED",
            Severity::High,
            Category::FontIntegrity,
            "rendered document",
            format!("OCR of the webview render confirms the rendered text diverges from the extracted text: {list}"),
            0.9,
        ));
    } else if sim >= 0.9 {
        report.verdict = Some(Verdict::Refuted);
        for f in report.findings.iter_mut().filter(|f| f.rule.contains("GLYPH_SEMANTIC_REPLACEMENT")) {
            f.severity = Severity::Info;
            f.message.push_str(
                " — OCR-refuted: the rendered document matches the extracted text (the font carries a glyph collision but it is not used on the visible text)",
            );
        }
    } // else: inconclusive OCR → stays Unconfirmed
    Ok(())
}

/// A per-process-unique temp path for the intermediate render PNG.
#[cfg(feature = "ocr-tesseract")]
fn render_temp_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("chk_defaced_render_{}_{n}.png", std::process::id()))
}

/// Load a rendered PNG as grayscale and OCR it as a text block.
#[cfg(feature = "ocr-tesseract")]
fn ocr_png(path: &Path, ocr: &dyn crate::ocr::OcrEngine) -> Result<String> {
    use crate::ocr::{GrayImage, OcrHint};
    let luma = image::open(path).with_context(|| format!("opening render {}", path.display()))?.to_luma8();
    let (w, h) = (luma.width(), luma.height());
    let img = GrayImage::new(w, h, luma.into_raw());
    ocr.recognize(&img, OcrHint::Block)
}

/// Insert `snippet` just before `</body>` (or append if there is no body close tag).
fn inject_before_body_end(html: &str, snippet: &str) -> String {
    match html.rfind("</body>") {
        Some(i) => {
            let mut s = String::with_capacity(html.len() + snippet.len());
            s.push_str(&html[..i]);
            s.push_str(snippet);
            s.push_str(&html[i..]);
            s
        }
        None => format!("{html}{snippet}"),
    }
}
