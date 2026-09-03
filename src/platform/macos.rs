pub fn focus_window() {
    let script = format!(
        "tell application \"System Events\" to set frontmost of (first process whose unix id is {}) to true",
        std::process::id()
    );
    let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
}

pub fn open_path(target: &str) {
    let _ = std::process::Command::new("open").arg(target).spawn();
}
