//! Binary entrypoint for `rsgit`.
//!
//! This file intentionally only handles bootstrap concerns: configuration,
//! listener creation, and handing accepted sockets to the application layer.

mod app;
mod config;
mod error;
mod git;
mod html;
mod http;
mod repo;
mod security;

use std::net::TcpListener;

use app::App;
use config::Config;
use error::Result;

fn main() -> Result<()> {
    let config = Config::from_env()?;
    let listener = TcpListener::bind(config.addr())?;
    eprintln!("rsgit listening on http://{}", config.addr());
    eprintln!("repository root: {}", config.repo_root().display());

    let app = App::new(config);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = app.handle_connection(stream) {
                    eprintln!("request error: {err}");
                }
            }
            Err(err) => eprintln!("connection error: {err}"),
        }
    }

    Ok(())
}
