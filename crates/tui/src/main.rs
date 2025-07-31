mod app;
mod client;
mod common;
mod render;
mod state;
mod widgets;
mod worker;

use crate::app::App;
use std::io;

// const VERSION: &str = env!("CARGO_PKG_VERSION");
fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let app_result = App::default().run(&mut terminal);
    ratatui::restore();
    app_result
}
