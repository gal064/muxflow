use std::io::{self, BufRead, Write};

fn main() {
    let name = std::env::args()
        .next()
        .and_then(|value| {
            std::path::Path::new(&value)
                .file_name()
                .map(|v| v.to_owned())
        })
        .and_then(|value| value.to_str().map(str::to_owned))
        .unwrap_or_else(|| "agent".to_owned());
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if !arguments.is_empty() {
        println!("fixture {name} resumed with {}", arguments.join(" "));
    }
    idle(&name);
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        match line.trim() {
            "working" => {
                if name.starts_with("claude") {
                    println!("Claude is working — Esc to interrupt");
                } else {
                    println!("Working (press Esc to interrupt)");
                }
            }
            "blocked" => {
                if name.starts_with("claude") {
                    println!("Do you want to proceed? Permission request");
                } else {
                    println!("Permission required: press enter to confirm");
                }
            }
            "idle" => idle(&name),
            "unknown" => println!("fixture state intentionally has no lifecycle marker"),
            "done" => {
                println!("fixture turn complete");
                idle(&name);
            }
            "exit" => break,
            other => println!("fixture input: {other}"),
        }
        let _ = io::stdout().flush();
    }
}

fn idle(name: &str) {
    if name.starts_with("claude") {
        println!("How can I help you today?\n❯ ");
    } else {
        println!("What would you like to do?\n› ");
    }
    let _ = io::stdout().flush();
}
