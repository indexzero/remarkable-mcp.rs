//! `remarkable-mcp` — an MCP server for the reMarkable cloud.
//!
//! A thin binary wrapper around [`remarkable_core`] (the rust-cli-aspects lib/bin
//! split): it parses argv, wires logging, and either runs an auth flow or serves
//! the MCP protocol over stdio.

mod config;
mod response;
mod scope;
mod server;

use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use remarkable_core::{ClientConfig, CloudClient};
use rmcp::transport::stdio;
use rmcp::ServiceExt;

use crate::config::ServerConfig;
use crate::server::RemarkableServer;

/// An MCP server for the reMarkable cloud: browse, search, and organize your tablet.
#[derive(Parser)]
#[command(name = "remarkable-mcp", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Register this machine with a one-time pairing code from
    /// https://my.remarkable.com/device/desktop/connect
    Auth {
        /// The 8-character one-time code.
        code: String,
    },
    /// Print authentication status and exit.
    Status,
    /// Run the MCP server over stdio (this is the default when no command is given).
    Mcp,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // Logs MUST go to stderr: stdout is the MCP JSON-RPC transport.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("remarkable=info,warn")),
        )
        .init();

    let cli = Cli::parse();
    let client = Arc::new(
        CloudClient::new(ClientConfig::from_env()?).context("initializing reMarkable client")?,
    );

    match cli.command.unwrap_or(Command::Mcp) {
        Command::Auth { code } => run_auth(&client, &code).await,
        Command::Status => run_status(&client).await,
        Command::Mcp => run_server(client).await,
    }
}

async fn run_auth(client: &CloudClient, code: &str) -> anyhow::Result<()> {
    eprintln!("Registering device with reMarkable cloud…");
    client
        .register(code)
        .await
        .context("device registration failed")?;
    let status = client.auth_status().await;
    println!("✓ Registered. Device id: {}", status.device_id);
    println!("  Tokens stored at: {}", status.token_path);
    println!("  You can now run `remarkable-mcp` (no arguments) as an MCP server.");
    Ok(())
}

async fn run_status(client: &CloudClient) -> anyhow::Result<()> {
    let status = client.auth_status().await;
    println!("authenticated:        {}", status.authenticated);
    println!("device_id:            {}", status.device_id);
    println!("valid user token:     {}", status.has_valid_user_token);
    if let Some(exp) = status.user_token_expires {
        println!("user token expires:   {exp}");
    }
    println!("token path:           {}", status.token_path);
    if !status.authenticated {
        println!("\nNot registered. Run: remarkable-mcp auth <one-time-code>");
    }
    Ok(())
}

async fn run_server(client: Arc<CloudClient>) -> anyhow::Result<()> {
    let cfg = ServerConfig::from_env();
    tracing::info!("starting remarkable-mcp server over stdio");
    let handler = RemarkableServer::new(client, cfg);
    let service = handler
        .serve(stdio())
        .await
        .context("starting MCP stdio service")?;
    service.waiting().await.context("MCP service error")?;
    Ok(())
}
