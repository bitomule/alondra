//! The `alondra` binary's root: it installs the transport and the hooks the adapters reach each
//! other through, and hands each command to the adapter that owns it.
use std::sync::Arc;

use alondra_cli::args::{Cmd, MacCmd, ReportCmd};
use anyhow::Result;

const CATALOG: &str = include_str!(concat!(env!("OUT_DIR"), "/log-catalog.txt"));
const VOCABULARY: &str = include_str!(concat!(env!("OUT_DIR"), "/vocabulary.txt"));

pub fn main() -> Result<()> {
    wire();
    alondra_cli::args::main(run)
}

fn wire() {
    alondra_client_transport::install();
    alondra_client_port::account::install(Arc::new(alondra_cli::account::Keychain));
    alondra_client_port::report::install_diagnosis(alondra_cli::doctor::diagnosis);
    alondra_cli_report::install_catalog(CATALOG, VOCABULARY);
}

fn run(command: Option<Cmd>, json: bool, no_input: bool) -> Result<()> {
    match command {
        None => alondra_cli::doctor::launch(alondra_cli_tui::run),
        Some(Cmd::Report { window, description, kind, app_section, without, command }) => {
            use alondra_cli_report as report;
            match command {
                None => report::run(&window, description, app_section.as_deref(), &without, kind, json, no_input),
                Some(ReportCmd::Save { id, file }) => {
                    let file = file.unwrap_or_else(|| report::default_file_name(&id).into());
                    report::save(&report::Mac::new(), &id, &file)?;
                    println!("{}", report::saved_notice(&file));
                    Ok(())
                }
                Some(ReportCmd::Send { id, kind, retry }) => report::send_report(&id, kind, retry, json),
                Some(ReportCmd::Keep { id }) => report::keep_report(&id),
                Some(ReportCmd::List) => report::list_reports(json),
                Some(ReportCmd::Open { target }) => report::open_report(&target),
                Some(ReportCmd::Delete { target }) => report::delete_report(&target),
            }
        }
        Some(Cmd::McpNode) => alondra_cli_mcp::serve_node(),
        Some(Cmd::McpOrchestrator) => alondra_cli_mcp::serve_orchestrator(),
        Some(Cmd::Mac { command: MacCmd::Ping { mac, count } }) => alondra_client_transport::ping(mac.as_deref(), count, json),
        Some(command) => alondra_cli::args::run(command, json, no_input),
    }
}

/// The report's final check of `files`, with the catalog this binary was built with.
pub fn report_leaks(files: &[(&str, &str)], values: &[String]) -> Vec<alondra_cli_report::canary::Hit> {
    alondra_cli_report::install_catalog(CATALOG, VOCABULARY);
    alondra_cli_report::leaks(files, values)
}

pub fn private_values_of_this_mac() -> Vec<String> {
    alondra_cli_report::install_catalog(CATALOG, VOCABULARY);
    alondra_cli_report::private_values_of_this_mac()
}
