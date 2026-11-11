//! Live stream: the daemon's Server-Sent Events feed (`GET /board/live`).
//! One reader per fleet panel keeps the board fresh without polling.

use std::io::{BufRead, BufReader};
use std::time::Duration;

use gentle_core::orchestrator::Board;

/// Drives the stream in the background: reconnects on EOF and yields parsed
/// board events through the callback. `heartbeat` comments are ignored; a
/// closed stream reconnects after the retry delay, so a daemon restart is
/// transparent to the UI.
pub fn spawn_stream<F>(base: String, mut on_board: F)
where
	F: FnMut(Board) + Send + 'static,
{
	std::thread::spawn(move || {
		loop {
			let url = format!("{base}/board/live");
			let agent: ureq::Agent = ureq::Agent::config_builder()
				.http_status_as_error(false)
				.timeout_global(Some(Duration::from_secs(120)))
				.build()
				.into();
			let response = agent.get(&url).call();
			match response {
				Ok(mut resp) if resp.status().as_u16() == 200 => {
					let reader = BufReader::new(resp.body_mut().as_reader());
					let mut event_name = String::new();
					for line in reader.lines() {
						let line = match line {
							Ok(line) => line,
							Err(_) => break,
						};
						if line.starts_with("event:") {
							event_name = line.trim_start_matches("event:").trim().to_string();
						} else if line.starts_with("data:") && event_name == "board" {
							let payload = line.trim_start_matches("data:").trim();
							if let Ok(board) = serde_json::from_str::<Board>(payload) {
								on_board(board);
							}
							event_name.clear();
						} else if line.is_empty() {
							event_name.clear();
						}
					}
				}
				_ => {}
			}
			std::thread::sleep(Duration::from_secs(2));
		}
	});
}
