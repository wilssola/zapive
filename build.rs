fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_bundled_translations("i18n"),
    )
    .expect("failed to compile ui/app.slint");

    // The static FFmpeg from vcpkg references DirectShow/MediaFoundation
    // GUIDs that live in Windows SDK import libraries ffmpeg-sys does not
    // emit on its own.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        for lib in ["strmiids", "mfuuid", "uuid", "ole32", "oleaut32"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
