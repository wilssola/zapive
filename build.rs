fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .with_bundled_translations("i18n")
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("failed to compile ui/app.slint");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_windows_resources();
        stage_ctranslate2();
        delay_load_ctranslate2();
    }
}

// ctranslate2.dll is delay-loaded: the process starts even when the DLL is
// missing (an install updated by an older self-updater never received it),
// and src/translation/engine.rs checks that it can be loaded before the
// first call into it, so translation reports an error instead of the whole
// app failing at launch.
fn delay_load_ctranslate2() {
    println!("cargo:rustc-link-lib=delayimp");
    println!("cargo:rustc-link-arg=/DELAYLOAD:ctranslate2.dll");
}

const CT2_VERSION: &str = "4.8.4";
const CT2_RUNTIME: &[&str] = &["ctranslate2.dll", "libiomp5md.dll"];

// Windows links the official CTranslate2 DLL (scripts/fetch-ctranslate2.ps1)
// and the executable needs it, with the OpenMP runtime, right next to it, so
// copy both beside the built exe for `cargo run` and `cargo test`.
fn stage_ctranslate2() {
    use std::path::PathBuf;
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let vendor = manifest
        .join("vendor")
        .join(format!("ctranslate2-{CT2_VERSION}"));
    println!("cargo:rerun-if-changed={}", vendor.display());
    println!("cargo:rerun-if-changed=build.rs");
    if !vendor.join("ctranslate2.lib").is_file() {
        panic!(
            "{} is missing: run scripts/fetch-ctranslate2.ps1 first (downloads the official CTranslate2 {CT2_VERSION} build)",
            vendor.display()
        );
    }
    // OUT_DIR = target/<profile>/build/<pkg>-<hash>/out, the exe lands in target/<profile>/.
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("out dir"));
    let Some(profile) = out.ancestors().nth(3) else {
        return;
    };
    for dll in CT2_RUNTIME {
        let (from, to) = (vendor.join(dll), profile.join(dll));
        let stale = std::fs::metadata(&to).map(|m| m.len()).ok()
            != std::fs::metadata(&from).map(|m| m.len()).ok();
        if stale {
            std::fs::copy(&from, &to)
                .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
        }
        // `cargo test` runs the test executable from target/<profile>/deps.
        let deps = profile.join("deps").join(dll);
        if deps.parent().is_some_and(|d| d.is_dir()) {
            let _ = std::fs::copy(&from, deps);
        }
    }
}

// Application icon and version info on the executable itself.
#[cfg(windows)]
fn embed_windows_resources() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("ui/zapive.ico");
    res.set("ProductName", "Zapive");
    res.set("FileDescription", "Zapive");
    if let Err(e) = res.compile() {
        println!("cargo:warning=winresource failed: {e}");
    }
}

#[cfg(not(windows))]
fn embed_windows_resources() {}
