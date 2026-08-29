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

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

//! Reads Amaru's JSON trace stream and projects its operator-facing state onto
//! a compact Bluetooth Low Energy GATT characteristic.

#[cfg(target_os = "linux")]
mod ble;
mod journal;
mod power_off;
mod projection;
mod system;
mod trace;
mod wire;

use std::{path::PathBuf, sync::Arc, time::Duration};

#[cfg(target_os = "linux")]
use anyhow::Context;
use clap::Parser;
use journal::TraceLine;
#[cfg(target_os = "linux")]
use journal::follow;
use projection::Projection;
use tokio::sync::{Mutex, mpsc, watch};
use trace::Record;
use wire::{fragment, snapshot_bytes};

const DEFAULT_JOURNAL_CURSOR_FILE: &str = "/var/lib/amaru-mobile-telemetry/journal.cursor";
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Projects Amaru's public telemetry to a deliberately narrow BLE GATT service.
///
/// The options avoid assuming a particular systemd installation while keeping
/// the default path safe for the common one-node host.
#[derive(Debug, Parser)]
#[command(name = "amaru-mobile-telemetry")]
struct Args {
    /// systemd unit to follow, so the bridge does not read unrelated journal entries.
    #[arg(long, env = "AMARU_MOBILE_JOURNAL_UNIT", default_value = "amaru.service")]
    journal_unit: String,

    /// File holding the last journal cursor, making restarts incremental rather than lossy.
    #[arg(long, env = "AMARU_MOBILE_JOURNAL_CURSOR_FILE", default_value = DEFAULT_JOURNAL_CURSOR_FILE)]
    journal_cursor_file: PathBuf,

    /// Network label shown before a tip trace can establish live chain state.
    #[arg(long, env = "AMARU_NETWORK", default_value = "mainnet")]
    network: String,

    /// Amaru process to sample, avoiding ambiguous process-name discovery on multi-node hosts.
    #[arg(long, env = "AMARU_PID")]
    pid: Option<u32>,

    /// Bluetooth adapter to serve, allowing operators to reserve another adapter for other roles.
    #[arg(long, env = "AMARU_BLUETOOTH_ADAPTER")]
    adapter: Option<String>,

    /// Local name advertised over Bluetooth so a nearby node is recognisable to a human.
    #[arg(long, env = "AMARU_MOBILE_BLUETOOTH_NAME", default_value = "Amaru")]
    bluetooth_name: String,

    /// Expose the fixed, unauthenticated power-off characteristic only when its availability trade-off is accepted.
    #[arg(long, env = "AMARU_MOBILE_ENABLE_POWER_OFF", default_value_t = false)]
    enable_power_off: bool,
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let pid = system::resolve_pid(args.pid).context("resolve Amaru process")?;
    let version = system::amaru_version().await.context("read Amaru version")?;
    let projection = Arc::new(Mutex::new(Projection::new(args.network, pid.as_u32(), version)));
    let (trace_tx, trace_rx) = mpsc::channel(4_096);
    let (snapshot_tx, snapshot_rx) = watch::channel(Vec::new());

    let journal_unit = args.journal_unit.clone();
    let journal_cursor_file = args.journal_cursor_file.clone();
    tokio::spawn(async move {
        if let Err(error) = follow(journal_unit, journal_cursor_file, trace_tx).await {
            eprintln!("amaru-mobile-telemetry: journal follower stopped: {error:#}");
        }
    });

    let power_off_enabled = args.enable_power_off;
    let publisher = tokio::spawn(publish(projection, trace_rx, snapshot_tx, power_off_enabled));
    let ble = ble::serve(snapshot_rx, args.adapter.as_deref(), &args.bluetooth_name, args.enable_power_off).await;
    publisher.abort();
    ble
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("amaru-mobile-telemetry is only supported on Linux hosts running BlueZ")
}

/// Folds trace and host updates into one latest-value channel for the BLE task.
///
/// A watch channel intentionally discards obsolete snapshots: a dashboard gains
/// nothing from a backlog, while retaining one value preserves the radio and
/// memory budgets under a slow receiver.
async fn publish(
    projection: Arc<Mutex<Projection>>,
    mut trace_rx: mpsc::Receiver<TraceLine>,
    snapshot_tx: watch::Sender<Vec<Vec<u8>>>,
    power_off_enabled: bool,
) {
    let mut sampler = system::Sampler::new(projection.lock().await.pid());
    let mut sequence = 0_u32;
    let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            Some(line) = trace_rx.recv() => {
                if let Some(record) = Record::parse(&line.text) {
                    projection.lock().await.apply(record, line.at);
                }
            }
            _ = ticker.tick() => {
                let mut projection = projection.lock().await;
                projection.set_system_sample(sampler.sample());
                let bytes = snapshot_bytes(&projection, sequence, power_off_enabled);
                let _ = snapshot_tx.send(fragment(sequence, &bytes));
                sequence = sequence.wrapping_add(1);
            }
        }
    }
}
