use color_eyre::eyre::{Context, Result};
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};

use crate::agent::Agent;

/// 运行 CLI REPL 交互循环
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

                match agent.process_message(input).await {
                    Ok(reply) => {
                        println!("\n{}\n", reply);
                    }
                    Err(e) => {
                        eprintln!("\n错误: {:#}\n", e);
                    }
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

/// 单次消息模式（非交互）
pub async fn run_single(agent: &mut Agent, message: &str) -> Result<()> {
    match agent.process_message(message).await {
        Ok(reply) => {
            println!("{}", reply);
        }
        Err(e) => {
            eprintln!("错误: {:#}", e);
        }
    }
    Ok(())
}
