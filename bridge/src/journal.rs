// Copyright 2026 PRAGMA
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Incremental reader for JSON traces held by the system journal.

use std::{
    io,
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc::Sender,
};

use crate::trace::is_relevant;

const CURSOR_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const INITIAL_REPLAY_ENTRIES: usize = 4_096;
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// A trace line and the time recorded by the system journal.
///
/// The journal timestamp, rather than bridge receipt time, preserves rate and
/// rollback timing when the initial replay catches the projection up.
#[derive(Debug)]
pub struct TraceLine {
    pub at: SystemTime,
    pub text: String,
}

/// Follows JSON traces emitted by `unit` and checkpoints the latest journal cursor.
///
/// The bounded replay restores a useful projection after a bridge restart. The
/// cursor then advances only after a line has been forwarded, which avoids
/// skipping state while still preventing the next restart from replaying the
/// entire journal.
pub async fn follow(unit: String, cursor_path: PathBuf, sender: Sender<TraceLine>) -> anyhow::Result<()> {
    let mut cursor = Cursor::load(cursor_path).await?;

    // The projected dashboard state is intentionally in-memory only. Rebuild it from a bounded
    // recent window whenever the bridge starts, then use its final cursor to enter live follow.
    if let Err(error) = consume(command(&unit, None, false, Some(INITIAL_REPLAY_ENTRIES)), &mut cursor, &sender).await {
        let Some(previous) = cursor.value() else {
            return Err(error).context("replay recent journal entries");
        };
        eprintln!("amaru-mobile-telemetry: unable to replay recent journal entries, resuming saved cursor: {error:#}");
        consume(command(&unit, Some(previous), false, None), &mut cursor, &sender).await?;
    }
    cursor.flush().await?;

    loop {
        let after = cursor.value();
        match consume(command(&unit, after, true, None), &mut cursor, &sender).await {
            Ok(()) if sender.is_closed() => return Ok(()),
            Ok(()) => eprintln!("amaru-mobile-telemetry: journal follower exited unexpectedly; restarting"),
            Err(error) => eprintln!("amaru-mobile-telemetry: journal follower failed; restarting: {error:#}"),
        }

        cursor.flush().await?;
        tokio::time::sleep(RESTART_DELAY).await;
    }
}

fn command(unit: &str, after: Option<&str>, follow: bool, tail: Option<usize>) -> Command {
    let mut command = Command::new("journalctl");
    command
        .arg("--unit")
        .arg(unit)
        .arg("--output=json")
        .arg("--quiet")
        .arg("--no-pager")
        .stdout(Stdio::piped())
        .kill_on_drop(true);

    if let Some(cursor) = after {
        command.arg(format!("--after-cursor={cursor}"));
    } else if let Some(tail) = tail {
        command.arg(format!("--lines={tail}"));
    }

    if follow {
        command.arg("--follow");
    }

    command
}

async fn consume(mut command: Command, cursor: &mut Cursor, sender: &Sender<TraceLine>) -> anyhow::Result<()> {
    let mut child = command.spawn().context("spawn journalctl")?;
    let stdout = child.stdout.take().context("capture journalctl stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut flush = tokio::time::interval(CURSOR_FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            result = lines.next_line() => match result.context("read journalctl output")? {
                Some(line) => {
                    let Ok(entry) = serde_json::from_str::<JournalEntry>(&line) else {
                        continue;
                    };
                    let at = entry.recorded_at();
                    cursor.observe(entry.cursor);

                    if let Some(message) = entry.message
                        && is_relevant(&message)
                        && sender.send(TraceLine { at, text: message }).await.is_err()
                    {
                        return Ok(());
                    }
                }
                None => break,
            },
            _ = flush.tick() => cursor.flush().await?,
        }
    }

    cursor.flush().await?;
    let status = child.wait().await.context("wait for journalctl")?;
    anyhow::ensure!(status.success(), "journalctl exited with {status}");
    Ok(())
}

#[derive(Debug, Deserialize)]
struct JournalEntry {
    #[serde(rename = "__CURSOR")]
    cursor: String,
    #[serde(rename = "__REALTIME_TIMESTAMP")]
    recorded_at_micros: String,
    #[serde(rename = "MESSAGE")]
    message: Option<String>,
}

impl JournalEntry {
    fn recorded_at(&self) -> SystemTime {
        self.recorded_at_micros
            .parse::<u64>()
            .ok()
            .and_then(|micros| UNIX_EPOCH.checked_add(Duration::from_micros(micros)))
            .unwrap_or_else(SystemTime::now)
    }
}

/// Atomic checkpoint of the latest journal entry observed by the bridge.
struct Cursor {
    path: PathBuf,
    current: Option<String>,
    persisted: Option<String>,
}

impl Cursor {
    async fn load(path: PathBuf) -> anyhow::Result<Self> {
        let persisted = match tokio::fs::read_to_string(&path).await {
            Ok(value) => (!value.trim().is_empty()).then(|| value.trim().to_owned()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("read journal cursor"),
        };

        Ok(Self { path, current: persisted.clone(), persisted })
    }

    fn observe(&mut self, value: String) {
        self.current = Some(value);
    }

    fn value(&self) -> Option<&str> {
        self.current.as_deref()
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        if self.current == self.persisted {
            return Ok(());
        }

        let Some(value) = self.current.as_deref() else {
            return Ok(());
        };
        let parent = self.path.parent().context("journal cursor has no parent directory")?;
        tokio::fs::create_dir_all(parent).await.context("create journal cursor directory")?;

        let temporary = self.path.with_extension("tmp");
        tokio::fs::write(&temporary, value).await.context("write journal cursor")?;
        tokio::fs::rename(&temporary, &self.path).await.context("replace journal cursor")?;
        self.persisted = self.current.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_journal_entries() {
        let entry = serde_json::from_str::<JournalEntry>(
            r#"{"__CURSOR":"s=cursor","__REALTIME_TIMESTAMP":"1","MESSAGE":"{\"fields\":{}}"}"#,
        )
        .expect("journal entry");

        assert_eq!(entry.cursor, "s=cursor");
        assert_eq!(entry.message.as_deref(), Some(r#"{"fields":{}}"#));
        assert_eq!(entry.recorded_at(), UNIX_EPOCH + Duration::from_micros(1));
    }
}
