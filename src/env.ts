// Must run before slint-ui is imported. Skia has proper color-emoji font
// fallback on Windows; if this build of slint-ui doesn't ship it, Slint
// falls back to the default renderer with a console notice.
// Skia is the only renderer compiled into slint-ui's prebuilt binary and
// draws through OpenGL/Vulkan, so this keeps rendering on the GPU while
// also giving color-emoji font fallback.
process.env.SLINT_BACKEND ??= "winit-skia";
