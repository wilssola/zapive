fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_bundled_translations("i18n"),
    )
    .expect("failed to compile ui/app.slint");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_windows_resources();
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
