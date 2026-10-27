use slint::fontique_011::fontique::{self, GenericFamily};

static NOTO_EMOJI: &[u8] = include_bytes!("../ui/fonts/noto_color/NotoColorEmoji.ttf");

// Color emoji everywhere text is drawn. The bundled Noto Color Emoji goes
// right after the primary UI font in the sans-serif and system-ui stacks,
// ahead of the monochrome symbol fonts fontconfig sorts in after it; the
// primary keeps letters, digits and the line height (Slint measures that
// on the first font). It is also the emoji family parley falls back to.
// Must run before the first window is created.
pub fn install_emoji() {
    let mut collection = slint::fontique_011::shared_collection();
    let blob = fontique::Blob::new(std::sync::Arc::new(NOTO_EMOJI));
    let Some((emoji, _)) = collection.register_fonts(blob, None).into_iter().next() else {
        log::warn!("[fonts] the bundled emoji font did not register");
        return;
    };
    for generic in [GenericFamily::SansSerif, GenericFamily::SystemUi] {
        let mut stack: Vec<_> = collection.generic_families(generic).collect();
        stack.retain(|f| *f != emoji);
        stack.insert(stack.len().min(1), emoji);
        collection.set_generic_families(generic, stack.into_iter());
    }
    collection.set_generic_families(GenericFamily::Emoji, std::iter::once(emoji));
}
