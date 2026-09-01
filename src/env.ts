// Must run before slint-ui is imported. Skia has proper color-emoji font
// fallback on Windows; if this build of slint-ui doesn't ship it, Slint
// falls back to the default renderer with a console notice.
process.env.SLINT_BACKEND ??= "winit-skia";
