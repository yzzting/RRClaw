//! CLI client for connecting to the daemon via Unix socket.
//!
//! Provides a REPL (using reedline, matching the original `rrclaw agent` style)
//! that sends messages through the daemon's IPC socket and displays streaming
//! responses with a thinking animation.

use color_eyre::eyre::{eyre, Context, Result};
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};
use std::io::Write;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::protocol::{ClientMessage, DaemonMessage};

// ANSI colour helpers
const RESET: &str = "\x1b[0m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

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
    let writer = Arc::new(tokio::sync::Mutex::new(writer));

    let session_id = uuid::Uuid::new_v4().to_string();
    let lang = crate::config::Config::get_language();

    if lang.is_english() {
        println!(
            "{}RRClaw{}  AI assistant — daemon mode (type /help for commands, exit to quit)",
            CYAN, RESET
        );
    } else {
        println!(
            "{}RRClaw{} AI 助手 — daemon 模式（输入 /help 查看命令，exit 退出）",
            CYAN, RESET
        );
    }
    println!();

    // reedline REPL — same prompt style as `rrclaw agent`
    let mut line_editor = Reedline::create();
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("rrclaw".to_string()),
        DefaultPromptSegment::Empty,
    );

    loop {
        let sig = line_editor.read_line(&prompt);

        match sig {
            Ok(Signal::Success(input)) => {
                let input = input.trim().to_string();
                if input.is_empty() {
                    continue;
                }
                if input == "exit" || input == "quit" {
                    if lang.is_english() {
                        println!("Goodbye!");
                    } else {
                        println!("再见！");
                    }
                    break;
                }

                // Send message to daemon
                let msg = ClientMessage::Message {
                    session_id: session_id.clone(),
                    content: input.clone(),
                };
                {
                    let mut w = writer.lock().await;
                    let mut json = serde_json::to_string(&msg)?;
                    json.push('\n');
                    w.write_all(json.as_bytes()).await?;
                    w.flush().await?;
                }

                // Show thinking animation while waiting for first token
                let thinking_flag = Arc::new(AtomicBool::new(true));
                let thinking_flag_clone = thinking_flag.clone();
                let thinking_text = if lang.is_english() {
                    "Thinking..."
                } else {
                    "思考中..."
                };
                let mut thinking_handle = Some(tokio::spawn(async move {
                    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                    let mut i = 0usize;
                    while thinking_flag_clone.load(Ordering::Relaxed) {
                        print!(
                            "\r{}{}{}{}",
                            YELLOW,
                            frames[i % frames.len()],
                            thinking_text,
                            RESET
                        );
                        let _ = std::io::stdout().flush();
                        i += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    }
                }));

                // Read streaming response from daemon
                let mut first_token = true;
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let daemon_msg: DaemonMessage = serde_json::from_str(&line)
                                .wrap_err("Failed to parse daemon message")?;

                            match daemon_msg {
                                DaemonMessage::Token { content } => {
                                    // Stop thinking animation on first token
                                    if first_token {
                                        thinking_flag.store(false, Ordering::Relaxed);
                                        if let Some(h) = thinking_handle.take() {
                                            let _ = h.await;
                                        }
                                        print!("\r\x1b[K"); // clear thinking line
                                        let _ = std::io::stdout().flush();
                                        first_token = false;
                                    }
                                    print!("{}", content);
                                    let _ = std::io::stdout().flush();
                                }
                                DaemonMessage::Done => {
                                    // Stop thinking animation if no tokens received
                                    if first_token {
                                        thinking_flag.store(false, Ordering::Relaxed);
                                        if let Some(h) = thinking_handle.take() {
                                            let _ = h.await;
                                        }
                                        print!("\r\x1b[K");
                                        let _ = std::io::stdout().flush();
                                    }
                                    println!("\n");
                                    break;
                                }
                                DaemonMessage::Error { message } => {
                                    thinking_flag.store(false, Ordering::Relaxed);
                                    if let Some(h) = thinking_handle.take() {
                                        let _ = h.await;
                                    }
                                    print!("\r\x1b[K");
                                    eprintln!("\n[error] {}\n", message);
                                    break;
                                }
                                DaemonMessage::Confirm {
                                    request_id,
                                    tool,
                                    args,
                                } => {
                                    thinking_flag.store(false, Ordering::Relaxed);
                                    if let Some(h) = thinking_handle.take() {
                                        let _ = h.await;
                                    }
                                    print!("\r\x1b[K");

                                    let args_str = serde_json::to_string_pretty(&args)
                                        .unwrap_or_else(|_| format!("{:?}", args));
                                    println!(
                                        "\n{}[confirm]{} Tool '{}' wants to execute:\n{}",
                                        YELLOW, RESET, tool, args_str
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
                                    first_token = true; // reset for next response
                                }
                            }
                        }
                        Ok(None) => {
                            return Err(eyre!("Daemon disconnected unexpectedly"));
                        }
                        Err(e) => {
                            return Err(e).wrap_err("Error reading from daemon");
                        }
                    }
                }
            }
            Ok(Signal::CtrlC) => {
                println!();
                continue; // interrupt current input, go back to prompt
            }
            Ok(Signal::CtrlD) => {
                if lang.is_english() {
                    println!("\nGoodbye!");
                } else {
                    println!("\n再见！");
                }
                break;
            }
            Err(e) => {
                eprintln!("Input error: {}", e);
                break;
            }
        }
    }

    Ok(())
}
