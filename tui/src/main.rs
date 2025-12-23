mod cli;
mod gtk_app;
mod theme;

#[cfg(feature = "tips")]
mod tips;

use clap::Parser;

fn main() {
    let args = cli::Args::parse();
    if let Err(err) = gtk_app::run(args) {
        eprintln!("linutil: {err}");
    }
}
