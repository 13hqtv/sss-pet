// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Check for the `mcp-server` subcommand. When launched with this
    // arg, run the MCP JSON-RPC server over stdio instead of the Tauri
    // GUI app. Agent CLIs (Claude/Codex/etc.) spawn swd-pet.exe with
    // `["mcp-server"]` as an MCP subprocess.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "mcp-server" {
        swd_pet_lib::logger::init(false); // no stderr — stdout is JSON-RPC
        log::info!("starting swd-pet MCP server");
        swd_pet_lib::mcp_server::run_mcp_server();
        return;
    }
    swd_pet_lib::run()
}
