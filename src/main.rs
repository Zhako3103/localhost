mod cgi;
mod config;
mod epoll;
mod handler;
mod http;
mod router;
mod server;
mod session;

use std::process::exit;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/server.conf".to_string());

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read config '{}': {}", path, e);
            exit(1);
        }
    };
    let cfg = match config::parse(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {}", e);
            exit(1);
        }
    };
    if let Err(e) = config::validate(&cfg, true) {
        eprintln!("invalid configuration: {}", e);
        exit(1);
    }
    if let Err(e) = server::run(cfg) {
        eprintln!("fatal: {}", e);
        exit(1);
    }
}
