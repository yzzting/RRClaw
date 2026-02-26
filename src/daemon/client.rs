//! CLI client for connecting to the daemon via Unix socket.
//!
//! Provides a REPL that sends messages through the daemon's IPC socket
//! and displays streaming responses.

use color_eyre::eyre::{eyre, Context, Result};
use std::io::Write;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::protocol::{ClientMessage, DaemonMessage};

/// `rrclaw chat` — connect to daemon and start interactive REPL.
pub async fn run_chat() -> Result<()> {
    let sock_path = super::sock_path()?;

    if !sock_path.exists() {
        println!("Daemon not running. Start it with `rrclaw start`.");
        return Ok(());
    }

    let stream = UnixStream::connect(&sock_path)
        .await
        .wrap_err("Failed to connect to daemon. Is it running?")?;

    let (reader, writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));

    let session_id = uuid::Uuid::new_v4().to_string();

    println!("Connected to RRClaw daemon. Type 'exit' or Ctrl-D to quit.\n");

    loop {
        // Print prompt
        print!("you> ");
        std::io::stdout().flush()?;

        // Read user input
        let mut input = String::new();
        let n = std::io::stdin().read_line(&mut input)?;
        if n == 0 {
            // EOF (Ctrl-D)
            println!();
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if input == "exit" || input == "quit" {
            break;
        }

        // Send message to daemon
        let msg = ClientMessage::Message {
            session_id: session_id.clone(),
            content: input.to_string(),
        };
        {
            let mut w = writer.lock().await;
            let mut json = serde_json::to_string(&msg)?;
            json.push('\n');
            w.write_all(json.as_bytes()).await?;
            w.flush().await?;
        }

        // Read response from daemon
        print!("\nassistant> ");
        std::io::stdout().flush()?;

        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let daemon_msg: DaemonMessage =
                        serde_json::from_str(&line).wrap_err("Failed to parse daemon message")?;

                    match daemon_msg {
                        DaemonMessage::Token { content } => {
                            print!("{}", content);
                            std::io::stdout().flush()?;
                        }
                        DaemonMessage::Done => {
                            println!("\n");
                            break;
                        }
                        DaemonMessage::Error { message } => {
                            println!("\n[error] {}\n", message);
                            break;
                        }
                        DaemonMessage::Confirm {
                            request_id,
                            tool,
                            args,
                        } => {
                            // Show confirmation prompt
                            println!(
                                "\n[confirm] Tool '{}' wants to execute: {}",
                                tool,
                                serde_json::to_string_pretty(&args)
                                    .unwrap_or_else(|_| format!("{:?}", args))
                            );
                            print!("Allow? [y/N] ");
                            std::io::stdout().flush()?;

                            let mut response = String::new();
                            std::io::stdin().read_line(&mut response)?;
                            let approved = response.trim().eq_ignore_ascii_case("y");

                            let confirm_msg = ClientMessage::ConfirmResponse {
                                request_id,
                                approved,
                            };
                            let mut w = writer.lock().await;
                            let mut json = serde_json::to_string(&confirm_msg)?;
                            json.push('\n');
                            w.write_all(json.as_bytes()).await?;
                            w.flush().await?;
                        }
                    }
                }
                Ok(None) => {
                    return Err(eyre!("Daemon disconnected"));
                }
                Err(e) => {
                    return Err(e).wrap_err("Error reading from daemon");
                }
            }
        }
    }

    println!("Disconnected from daemon.");
    Ok(())
}
