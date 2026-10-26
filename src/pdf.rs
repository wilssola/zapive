// In-app PDF rendering through pdfium. Pdfium is not thread-safe, so one
// worker thread owns the library and the open document; the UI sends it
// requests and gets pages back as RGBA through bridge::ui_apply. The
// library itself is a shared object bound at run time: next to the
// executable, in the data directory (where the app downloads it on
// request), or wherever the system keeps one.
use crate::media::Decoded;
use crate::paths::data_dir;
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub enum PdfReq {
    // Loads a document; `generation` tags every answer so a viewer that moved
    // on can drop late pages.
    Open { path: PathBuf, generation: u64 },
    // Renders one page at the given pixel width.
    Render { page: usize, width: u32, generation: u64 },
    Close,
    // The engine was (re)installed; bind again on the next request.
    Rebind,
}

#[derive(Clone)]
pub struct PdfWorker {
    tx: mpsc::Sender<PdfReq>,
}

// Where the downloaded engine lives.
pub fn engine_dir() -> PathBuf {
    data_dir().join("pdfium")
}

fn platform_library_name() -> &'static str {
    if cfg!(windows) {
        "pdfium.dll"
    } else if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else {
        "libpdfium.so"
    }
}

// The release asset for this machine at bblanchon/pdfium-binaries.
pub fn engine_asset() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "pdfium-win-x64.tgz",
        ("windows", "aarch64") => "pdfium-win-arm64.tgz",
        ("macos", "aarch64") => "pdfium-mac-arm64.tgz",
        ("macos", "x86_64") => "pdfium-mac-x64.tgz",
        ("linux", "x86_64") => "pdfium-linux-x64.tgz",
        ("linux", "aarch64") => "pdfium-linux-arm64.tgz",
        _ => return None,
    })
}

pub fn engine_installed() -> bool {
    engine_dir().join(platform_library_name()).exists()
}

// Downloads and unpacks the engine into the data directory.
pub fn install_engine() -> Result<(), String> {
    let asset = engine_asset().ok_or_else(|| "unsupported platform".to_string())?;
    let url = format!(
        "https://github.com/bblanchon/pdfium-binaries/releases/latest/download/{asset}"
    );
    let mut res = ureq::get(&url)
        .header("User-Agent", "Zapive")
        .call()
        .map_err(|e| e.to_string())?;
    let bytes = res.body_mut().read_to_vec().map_err(|e| e.to_string())?;
    let dir = engine_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(gz);
    let wanted = platform_library_name();
    let mut found = false;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        // The archive holds bin/pdfium.dll (or lib/libpdfium.so); only the
        // library itself matters.
        if path.file_name().and_then(|n| n.to_str()) == Some(wanted) {
            let out = dir.join(wanted);
            let mut file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
            found = true;
        }
    }
    if found { Ok(()) } else { Err(format!("{wanted} not in the archive")) }
}

fn bind() -> Option<&'static Pdfium> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.to_path_buf());
    }
    candidates.push(engine_dir());
    for dir in candidates {
        let name = Pdfium::pdfium_platform_library_name_at_path(&dir);
        if let Ok(bindings) = Pdfium::bind_to_library(name) {
            // Leaked on purpose: the document borrows the library for as
            // long as the worker lives, which is the whole process.
            return Some(Box::leak(Box::new(Pdfium::new(bindings))));
        }
    }
    Pdfium::bind_to_system_library().ok().map(|b| &*Box::leak(Box::new(Pdfium::new(b))))
}

impl PdfWorker {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<PdfReq>();
        std::thread::Builder::new()
            .name("zapive-pdf".into())
            .spawn(move || worker(rx))
            .expect("pdf worker thread");
        Self { tx }
    }

    pub fn send(&self, req: PdfReq) {
        let _ = self.tx.send(req);
    }
}

fn worker(rx: mpsc::Receiver<PdfReq>) {
    let mut pdfium: Option<&'static Pdfium> = None;
    let mut doc: Option<PdfDocument<'static>> = None;
    let mut current_gen = 0u64;
    while let Ok(req) = rx.recv() {
        match req {
            PdfReq::Rebind => {
                doc = None;
                pdfium = None;
            }
            PdfReq::Close => {
                doc = None;
            }
            PdfReq::Open { path, generation } => {
                current_gen = generation;
                doc = None;
                if pdfium.is_none() {
                    pdfium = bind();
                }
                let Some(lib) = pdfium else {
                    crate::bridge::ui_apply(move |b| b.on_pdf_engine_missing(generation));
                    continue;
                };
                match lib.load_pdf_from_file(&path, None) {
                    Ok(loaded) => {
                        let sizes: Vec<(f32, f32)> = loaded
                            .pages()
                            .iter()
                            .map(|p| (p.width().value, p.height().value))
                            .collect();
                        doc = Some(loaded);
                        crate::bridge::ui_apply(move |b| b.on_pdf_opened(generation, sizes));
                    }
                    Err(e) => {
                        let reason = e.to_string();
                        crate::bridge::ui_apply(move |b| b.on_pdf_failed(generation, &reason));
                    }
                }
            }
            PdfReq::Render { page, width, generation } => {
                if generation != current_gen {
                    continue;
                }
                let Some(open) = doc.as_ref() else { continue };
                let rendered = render_page(open, page, width);
                match rendered {
                    Some(img) => crate::bridge::ui_apply(move |b| b.on_pdf_page(generation, page, img)),
                    None => eprintln!("[pdf] page {page} failed to render"),
                }
            }
        }
    }
}

fn render_page(doc: &PdfDocument<'_>, index: usize, width: u32) -> Option<Decoded> {
    let page = doc.pages().get(PdfPageIndex::from(index as u16)).ok()?;
    let config = PdfRenderConfig::new()
        .set_target_width(width.clamp(64, 4096) as i32)
        .render_form_data(true)
        .render_annotations(true);
    let bitmap = page.render_with_config(&config).ok()?;
    let rgba = bitmap.as_image().ok()?.to_rgba8();
    Some(Decoded { w: rgba.width(), h: rgba.height(), rgba: rgba.into_raw() })
}

// Whether a document message is a PDF the viewer can open.
pub fn is_pdf(mimetype: &str, name: &str) -> bool {
    mimetype.starts_with("application/pdf") || Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}
