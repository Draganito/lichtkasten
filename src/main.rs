mod albums;
mod app;
mod cache;
mod decode;
mod i18n;
mod index;
mod jobs;
mod library;
mod meta;
mod open_with;
mod orient;
mod raw;
mod theme;
mod thumbs;
mod trash;
mod video;

use app::Lichtkasten;

fn app_icon() -> egui::IconData {
    let image = image::load_from_memory(include_bytes!("../packaging/lichtkasten.png"))
        .expect("icon")
        .into_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

fn main() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("lichtkasten {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    i18n::init();
    video::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 820.0])
            .with_min_inner_size([800.0, 520.0])
            .with_title(format!("Lichtkasten {}", env!("CARGO_PKG_VERSION")))
            .with_app_id("lichtkasten")
            .with_icon(app_icon()),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "Lichtkasten",
        options,
        Box::new(|cc| Ok(Box::new(Lichtkasten::new(cc)))),
    )
}
