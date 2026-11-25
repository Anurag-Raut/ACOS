mod bridge;
mod grpc;
mod keymap;
mod live_client;
mod touch;

pub use grpc::pb;

use std::path::PathBuf;

use bridge::{serve_control_socket, BridgeControl};

fn main() {
    let mut socket_path: Option<PathBuf> = None;
    let mut callback_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket_path = args.next().map(PathBuf::from);
            }
            "--callback" => {
                callback_path = args.next().map(PathBuf::from);
            }
            other => {
                eprintln!("acos-devbridge: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(socket_path) = socket_path else {
        eprintln!("acos-devbridge: --socket <path> is required");
        std::process::exit(2);
    };
    let Some(callback_path) = callback_path else {
        eprintln!("acos-devbridge: --callback <path> is required");
        std::process::exit(2);
    };
    let shot_dir = socket_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("acos-devbridge")
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("acos-devbridge: cannot start tokio runtime: {e}");
            std::process::exit(1);
        }
    };

    rt.block_on(async move {
        let (control, shutdown_rx) = BridgeControl::new(callback_path, shot_dir);
        if let Err(err) = serve_control_socket(&socket_path, control, shutdown_rx).await {
            eprintln!("acos-devbridge: {err}");
            std::process::exit(1);
        }
    });
}
