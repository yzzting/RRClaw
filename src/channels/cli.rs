use color_eyre::eyre::{Context, Result};
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};
use std::io::Write;
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::providers::StreamEvent;

/// 运行 CLI REPL 交互循环（流式输出）
pub async fn run_repl(agent: &mut Agent) -> Result<()> {
    let mut line_editor = Reedline::create();
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("rrclaw".to_string()),
        DefaultPromptSegment::Empty,
    );

    println!("RRClaw AI 助手 (输入 exit 退出, clear 清屏)");
    println!();

    loop {
        let sig = line_editor.read_line(&prompt);
        match sig {
            Ok(Signal::Success(line)) => {
                let input = line.trim();

                if input.is_empty() {
                    continue;
                }

                match input {
                    "exit" | "quit" => {
                        println!("再见！");
                        break;
                    }
                    "clear" => {
                        line_editor.clear_scrollback().wrap_err("清屏失败")?;
                        continue;
                    }
                    _ => {}
                }

                print!("\n");
                if let Err(e) = stream_message(agent, input).await {
                    eprintln!("错误: {:#}\n", e);
                }
            }
            Ok(Signal::CtrlD) | Ok(Signal::CtrlC) => {
                println!("\n再见！");
                break;
            }
            Err(e) => {
                eprintln!("输入错误: {}", e);
                break;
            }
        }
    }

    Ok(())
}

/// 流式处理消息并实时打印
async fn stream_message(agent: &mut Agent, input: &str) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(64);

    // 在后台 task 中消费 stream events 并打印
    let print_handle = tokio::spawn(async move {
        let mut has_output = false;
        while let Some(event) = rx.recv().await {
            match event {
                StreamEvent::Text(text) => {
                    print!("{}", text);
                    let _ = std::io::stdout().flush();
                    has_output = true;
                }
                StreamEvent::Done(_) => {
                    // 流结束
                }
                StreamEvent::ToolCallDelta { .. } => {
                    // tool call 增量不打印给用户
                }
            }
        }
        has_output
    });

    // 调用流式处理
    let result = agent.process_message_stream(input, tx).await;

    // 等待打印完成
    let has_output = print_handle.await.unwrap_or(false);

    match result {
        Ok(_) => {
            if has_output {
                println!("\n");
            } else {
                println!();
            }
        }
        Err(e) => {
            println!();
            return Err(e);
        }
    }

    Ok(())
}

/// 单次消息模式（流式输出）
pub async fn run_single(agent: &mut Agent, message: &str) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(64);

    let print_handle = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let StreamEvent::Text(text) = event {
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
        }
    });

    let result = agent.process_message_stream(message, tx).await;
    let _ = print_handle.await;
    println!();

    if let Err(e) = result {
        eprintln!("错误: {:#}", e);
    }

    Ok(())
}
