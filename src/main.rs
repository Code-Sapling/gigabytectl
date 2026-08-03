//! gigabytectl — control panel for the `gigabyte-laptop-wmi` kernel module.
//!
//! Running without a subcommand starts the TUI; every subcommand is a one-shot,
//! scriptable command.

mod app;
mod cli;
mod config;
mod doctor;
mod history;
mod notify;
mod ppd;
mod sensors;
mod sysfs;
mod system;
mod tui;
mod ui;

use anyhow::Result;
use clap::Parser;

use crate::{cli::Cli, config::Config};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load();
    match cli.command {
        Some(command) => cli::run(command, &config),
        None => tui::run(config),
    }
}
