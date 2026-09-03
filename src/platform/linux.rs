pub fn focus_window() {
    let _ = std::process::Command::new("wmctrl").args(["-a", "Zapive"]).spawn();
}

pub fn open_path(target: &str) {
    let _ = std::process::Command::new("xdg-open").arg(target).spawn();
}
