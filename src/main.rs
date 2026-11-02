mod albums;
mod app;
mod cache;
mod decode;
mod jobs;
mod library;
mod meta;
mod orient;
mod raw;
mod theme;
mod thumbs;
mod video;

use app::Lichtkasten;

fn main() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("lichtkasten {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    video::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 820.0])
            .with_min_inner_size([800.0, 520.0])
            .with_title("Lichtkasten"),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "Lichtkasten",
        options,
        Box::new(|cc| Ok(Box::new(Lichtkasten::new(cc)))),
    )
}
