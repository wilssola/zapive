slint::include_modules!();

use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RT: OnceLock<Runtime> = OnceLock::new();

fn main() {
    slint::select_bundled_translation("pt").ok();

    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build the tokio runtime")
    });

    // Phase-0 spike: prove the WhatsApp stack initializes end to end.
    rt.spawn(async {
        match whatsapp_rust::store::SqliteStore::new("target/spike_wa.db").await {
            Ok(_) => println!("[spike] whatsapp-rust SqliteStore ready"),
            Err(e) => eprintln!("[spike] store init failed: {e}"),
        }
    });

    // Phase-0 spike: StyledText built from markdown, as the message bubbles need.
    match slint::StyledText::from_markdown("**bold** and [a link](jid)") {
        Ok(_) => println!("[spike] styled text ok"),
        Err(e) => eprintln!("[spike] styled text failed: {e:?}"),
    }

    let ui = AppWindow::new().expect("failed to create the main window");
    ui.set_screen("login".into());

    // Phase-0 spike: a tray icon living inside Slint's winit event loop.
    #[cfg(windows)]
    let _tray = {
        let tray = image::open("ui/zapive.png")
            .map_err(|e| e.to_string())
            .and_then(|img| {
                let rgba = img.into_rgba8();
                let (w, h) = rgba.dimensions();
                tray_icon::Icon::from_rgba(rgba.into_raw(), w, h).map_err(|e| e.to_string())
            })
            .and_then(|icon| {
                tray_icon::TrayIconBuilder::new()
                    .with_icon(icon)
                    .with_tooltip("Zapive")
                    .build()
                    .map_err(|e| e.to_string())
            });
        match &tray {
            Ok(_) => println!("[spike] tray ready"),
            Err(e) => eprintln!("[spike] tray failed: {e}"),
        }
        tray
    };

    ui.run().expect("event loop failed");
}
