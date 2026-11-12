//! `lexicue` binary entry point.
//!
//! Linux-first GUI terminal that resolves commands either exactly or from an
//! approximate description, without ever executing an inexact command silently.
//!
//! Ownership: this file only wires modules and starts the eframe event loop.

mod aliases;
mod app;
mod config;
mod contracts;
mod input;
mod pty;
mod resolver;
mod telemetry;
mod theme;
mod ui;

use crate::app::{LaunchOptions, LexicueApp};
use crate::config::Config;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
enum CliAction {
    Help,
    Version,
    Run {
        config_path: Option<PathBuf>,
        launch: LaunchOptions,
    },
}

fn parse_args_from<I>(iter: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = iter.into_iter();
    let mut config_path = None;
    let mut working_dir = None;
    let mut command = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(CliAction::Help),
            "-V" | "--version" => return Ok(CliAction::Version),
            "--config" => {
                let val = args
                    .next()
                    .ok_or_else(|| "missing value after --config".to_string())?;
                config_path = Some(PathBuf::from(val));
            }
            "-d" | "--working-dir" => {
                let val = args
                    .next()
                    .ok_or_else(|| format!("missing value after {arg}"))?;
                working_dir = Some(PathBuf::from(val));
            }
            "-e" | "--" => {
                let cmd: Vec<String> = args.collect();
                if cmd.is_empty() {
                    return Err(format!("missing command after {arg}"));
                }
                command = cmd;
                break;
            }
            _ if arg.starts_with("--config=") => {
                let val = &arg["--config=".len()..];
                if val.is_empty() {
                    return Err("empty value after --config=".to_string());
                }
                config_path = Some(PathBuf::from(val));
            }
            _ if arg.starts_with("--working-dir=") => {
                let val = &arg["--working-dir=".len()..];
                if val.is_empty() {
                    return Err("empty value after --working-dir=".to_string());
                }
                working_dir = Some(PathBuf::from(val));
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    Ok(CliAction::Run {
        config_path,
        launch: LaunchOptions {
            working_dir,
            command,
            startup_warning: None,
        },
    })
}

fn print_help() {
    println!(
        "lexicue {}\n\
         Usage: lexicue [OPTIONS] [-e <COMMAND> [ARGS...]]\n\n\
         Options:\n\
         \x20 -d, --working-dir <DIR>   Start shell in DIR\n\
         \x20     --config <PATH>       Use custom config.toml\n\
         \x20 -e, -- <COMMAND> [ARGS]   Run COMMAND instead of default shell\n\
         \x20 -h, --help                Print help\n\
         \x20 -V, --version             Print version",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (config_path, launch) = match parse_args_from(std::env::args().skip(1)) {
        Ok(CliAction::Help) => {
            print_help();
            return Ok(());
        }
        Ok(CliAction::Version) => {
            println!("lexicue {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Ok(CliAction::Run {
            config_path,
            launch,
        }) => (config_path, launch),
        Err(msg) => {
            eprintln!("lexicue: {msg}");
            std::process::exit(2);
        }
    };

    let mut launch = launch;
    let cfg = match Config::load(config_path.as_deref()) {
        Ok(c) => c,
        Err(e) if config_path.is_none() => {
            let msg = format!("config error (using defaults): {e}");
            eprintln!("lexicue: {msg}");
            launch.startup_warning = Some(msg);
            Config::default()
        }
        Err(e) => {
            eprintln!("lexicue: {e}");
            std::process::exit(1);
        }
    };

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 640.0])
            .with_title("Lexicue"),
        ..Default::default()
    };

    eframe::run_native(
        "lexicue",
        native_options,
        Box::new(move |cc| Ok(Box::new(LexicueApp::new(cc, cfg, config_path, launch)?))),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn parses_working_dir_config_and_command() {
        let parsed = parse_args_from(s(&[
            "--config=/tmp/c.toml",
            "-d",
            "/tmp",
            "-e",
            "htop",
            "-u",
            "root",
        ]))
        .expect("should parse");
        assert_eq!(
            parsed,
            CliAction::Run {
                config_path: Some(PathBuf::from("/tmp/c.toml")),
                launch: LaunchOptions {
                    working_dir: Some(PathBuf::from("/tmp")),
                    command: s(&["htop", "-u", "root"]),
                    startup_warning: None,
                },
            }
        );
    }

    #[test]
    fn parses_help_and_rejects_missing_command() {
        assert_eq!(parse_args_from(s(&["--help"])), Ok(CliAction::Help));
        assert!(parse_args_from(s(&["-e"])).is_err());
    }
}
