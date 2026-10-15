//! `simforge telemetry show`: what the installed runtime will report.
//!
//! Usage reporting is a condition of SimForge's account and download terms
//! (not of the runtime's PolyForm Noncommercial license), tied to the
//! signed-in account; there is no command to turn it off. The runtime keeps
//! the ledger and sends it; this command only asks the runtime to print the
//! pending batches exactly as they will be posted.

use clap::Subcommand;

use crate::contract::CmdResult;

#[derive(Debug, Subcommand)]
pub enum TelemetryCommand {
    /// Print the pending usage batches exactly as the runtime will send them.
    Show,
}

pub fn run(command: TelemetryCommand) -> CmdResult {
    match command {
        TelemetryCommand::Show => crate::runtime::telemetry(&["show".to_owned()]),
    }
}
