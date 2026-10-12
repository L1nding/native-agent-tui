//! Real native child for modeling protocol and process ownership tests.
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};
use std::time::Duration;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "descendant") {
        loop { std::thread::sleep(Duration::from_secs(60)); }
    }
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    assert!(lines.next().unwrap().unwrap().contains("start"));
    if let Some(path) = args.get(1) {
        let child = Command::new(std::env::current_exe().unwrap()).arg("descendant")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        std::fs::write(path, child.id().to_string()).unwrap();
    }
    println!(r#"{{"version":1,"run_id":"fixture","seq":1,"kind":"started","target":"八层"}}"#);
    println!(r#"{{"version":1,"run_id":"fixture","seq":2,"kind":"request","request_id":"q1","title":"层高确认","lines":["使用已确认的层高？"]}}"#);
    io::stdout().flush().unwrap();
    let answer = lines.next().unwrap().unwrap();
    assert!(answer.contains("answer") && answer.contains("fixture") && answer.contains("q1"));
    assert!(answer.contains("true"));
    println!(r#"{{"version":1,"run_id":"fixture","seq":3,"kind":"answered","request_id":"q1","accepted":true}}"#);
    println!(r#"{{"version":1,"run_id":"fixture","seq":4,"kind":"finished","outcome":"completed","message":"已复核"}}"#);
}
