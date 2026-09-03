fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_bundled_translations("i18n"),
    )
    .expect("failed to compile ui/app.slint");
}
