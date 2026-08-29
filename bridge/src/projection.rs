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

//! Bounded reduction of the trace stream into the mobile dashboard state.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant, SystemTime},
};

use amaru_observability::{
    RecordFields,
    amaru::{ledger, mempool, protocols},
};

use crate::trace::Record;

const PEER_LIMIT: usize = 32;
const PEER_STATE_LIMIT: usize = 128;
const PEER_IDLE_LIMIT: Duration = Duration::from_secs(600);
const RECENT_ROLLBACK_LIMIT: usize = 100;
const ROLLBACK_WINDOW: Duration = Duration::from_secs(600);
const SEEN_SPAN_LIMIT: usize = 4_096;
const RATE_SMOOTHING: usize = 10;

/// Compact resource values sampled locally from the running Amaru process.
///
/// Resource values are intentionally outside the trace projection: their
/// freshness comes from the bridge's one-second sampling tick, not from Amaru
/// activity, so an idle node can still report its health.
#[derive(Debug, Clone)]
pub struct ResourceSample {
    pub runtime_seconds: u64,
    pub cpu_percent: f64,
    pub process_memory_bytes: u64,
    pub rss_bytes: u64,
    pub virtual_bytes: u64,
    pub host_memory_used_bytes: u64,
    pub host_memory_total_bytes: u64,
    pub process_disk_read_bytes: u64,
    pub process_disk_write_bytes: u64,
    pub host_disk_read_bytes: u64,
    pub host_disk_write_bytes: u64,
    /// Mean of the finite component temperatures exposed by the host, in Celsius.
    pub average_temperature_celsius: Option<f32>,
    /// Highest finite component temperature exposed by the host, in Celsius.
    pub maximum_temperature_celsius: Option<f32>,
}

/// Values shown by the throughput card.
///
/// Totals start when the bridge starts. Rates are exponentially smoothed so a
/// single burst during catch-up does not dominate the operator display.
#[derive(Debug, Clone, Copy, Default)]
pub struct Throughput {
    pub blocks: u64,
    pub blocks_per_second: f64,
    pub transactions: u64,
    pub transactions_per_second: f64,
}

/// Values shown by the local tip card.
///
/// This remains optional until a typed `tip.update` event arrives; a missing
/// tip is therefore visible rather than guessed from process liveness.
#[derive(Debug, Clone)]
pub struct Tip {
    pub header_hash: String,
    pub slot: u64,
    pub block_height: u64,
    pub epoch: u64,
    pub slot_in_epoch: u64,
    pub density: f64,
    pub tx_count: u64,
}

/// Values shown by the chain-quality card over the bounded recent rollback window.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChainQuality {
    pub average_rollback_length: Option<f64>,
    pub rollback_frequency_per_second: Option<f64>,
}

/// Values shown by the mempool card, refreshed only when Amaru emits a mempool update.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mempool {
    pub transactions: u64,
    pub size_bytes: u64,
}

/// Compact peer state shown by the mobile peer table.
///
/// The address is the stable identity across connection and keepalive events;
/// the timestamp exists solely to evict stale entries from the bounded model.
#[derive(Debug, Clone)]
pub struct Peer {
    pub address: String,
    pub connected: bool,
    pub inbound: bool,
    pub outbound: bool,
    pub rtt_micros: Option<u64>,
    updated_at: Instant,
}

impl Peer {
    /// Starts a peer as unknown because a keepalive may arrive before a connection event.
    pub fn new(address: String) -> Self {
        Self {
            address,
            connected: false,
            inbound: false,
            outbound: false,
            rtt_micros: None,
            updated_at: Instant::now(),
        }
    }
}

/// Stateful and bounded reduction of the trace stream.
///
/// It retains only data needed to render the latest snapshot. Bounds on peers,
/// span identifiers, and rollback history prevent a long-lived bridge from
/// becoming a second trace store.
#[derive(Debug)]
pub struct Projection {
    network: String,
    pid: u32,
    version: String,
    tip: Option<Tip>,
    system_sample: Option<ResourceSample>,
    throughput: RateCounters,
    chain_quality: ChainQuality,
    mempool: Mempool,
    peers: BTreeMap<String, Peer>,
    recent_rollbacks: VecDeque<(SystemTime, usize)>,
    seen_spans: BTreeSet<u64>,
    seen_span_order: VecDeque<u64>,
}

impl Projection {
    /// Creates an empty projection with identity values discovered at bridge startup.
    pub fn new(network: String, pid: u32, version: String) -> Self {
        Self {
            network,
            pid,
            version,
            tip: None,
            system_sample: None,
            throughput: RateCounters::default(),
            chain_quality: ChainQuality::default(),
            mempool: Mempool::default(),
            peers: BTreeMap::new(),
            recent_rollbacks: VecDeque::new(),
            seen_spans: BTreeSet::new(),
            seen_span_order: VecDeque::new(),
        }
    }

    /// Returns the configured network label, which is available before a tip is observed.
    pub fn network(&self) -> &str {
        &self.network
    }

    /// Returns the process being sampled so the sampler and wire view cannot diverge.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Returns the startup version, avoiding a process invocation for every snapshot.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Updates host health and advances periodic projection maintenance.
    ///
    /// Uses the one-second sample as the model's maintenance tick so idle peers
    /// and old rollback events are evicted even when Amaru emits no traces.
    pub fn set_system_sample(&mut self, sample: Option<ResourceSample>) {
        self.system_sample = sample;
        self.throughput.sample();
        self.refresh_chain_quality();
        self.trim_peers();
    }

    /// Returns the most recent local sample, if the target process still exists.
    pub fn system_sample(&self) -> Option<&ResourceSample> {
        self.system_sample.as_ref()
    }

    /// Returns process-lifetime totals and smoothed recent rates for one snapshot.
    pub fn throughput(&self) -> Throughput {
        self.throughput.values()
    }

    /// Returns the latest observed tip without treating process startup as chain progress.
    pub fn tip(&self) -> Option<&Tip> {
        self.tip.as_ref()
    }

    /// Returns the bounded rollback view used to assess recent chain quality.
    pub fn chain_quality(&self) -> &ChainQuality {
        &self.chain_quality
    }

    /// Returns the last observed mempool state; no update means the prior state remains valid.
    pub fn mempool(&self) -> &Mempool {
        &self.mempool
    }

    /// Orders peers by responsiveness and truncates them before wire encoding.
    ///
    /// Sorting at snapshot time keeps mutation cheap while guaranteeing the
    /// constrained radio payload favours peers an operator can act on first.
    pub fn peers(&self) -> Vec<Peer> {
        let mut peers = self.peers.values().cloned().collect::<Vec<_>>();
        peers.sort_by_key(|peer| peer.rtt_micros.unwrap_or(u64::MAX));
        peers.truncate(PEER_LIMIT);
        peers
    }

    #[cfg(test)]
    pub fn insert_peer(&mut self, peer: Peer) {
        self.peers.insert(peer.address.clone(), peer);
    }

    /// Applies one JSON trace record emitted at `at`.
    ///
    /// Span identifiers are de-duplicated because journal replay and live
    /// following can overlap around the persisted cursor.
    pub fn apply(&mut self, record: Record, at: SystemTime) {
        if !self.accept_span(&record) {
            return;
        }
        let Some(name) = record.name() else {
            return;
        };

        if ledger::tip::UPDATE::matches(record.target(), name) {
            self.update_tip(&record, at);
        } else if ledger::state::SWITCH_TO_FORK::matches(record.target(), name) {
            if let Some(length) = record.usize(ledger::state::SWITCH_TO_FORK::FIELD_ROLLBACK_LENGTH) {
                self.recent_rollbacks.push_back((at, length));
                while self.recent_rollbacks.len() > RECENT_ROLLBACK_LIMIT {
                    self.recent_rollbacks.pop_front();
                }
            }
        } else if mempool::state::UPDATE::matches(record.target(), name) {
            self.mempool = Mempool {
                transactions: record.u64(mempool::state::UPDATE::FIELD_TX_COUNT).unwrap_or_default(),
                size_bytes: record.u64(mempool::state::UPDATE::FIELD_SIZE_BYTES).unwrap_or_default(),
            };
        } else if protocols::peer_selection::peer::CONNECTED::matches(record.target(), name) {
            self.peer_connected(&record);
        } else if protocols::peer_selection::peer::DISCONNECTED::matches(record.target(), name) {
            self.peer_disconnected(&record);
        } else if protocols::keepalive::peer::ROUND_TRIP::matches(record.target(), name) {
            self.peer_round_trip(&record);
        }
    }

    fn update_tip(&mut self, record: &Record, at: SystemTime) {
        let Some(tip) = tip(record) else {
            return;
        };

        if let Some(previous) = self.tip.as_ref() {
            let blocks = tip.block_height.saturating_sub(previous.block_height);
            if blocks > 0 {
                self.throughput.blocks.record(blocks, at);
                self.throughput.transactions.record(tip.tx_count, at);
            }
        }

        self.tip = Some(tip);
    }

    fn accept_span(&mut self, record: &Record) -> bool {
        let Some(id) = record.id() else {
            return true;
        };
        if !self.seen_spans.insert(id) {
            return false;
        }
        self.seen_span_order.push_back(id);
        if self.seen_span_order.len() > SEEN_SPAN_LIMIT
            && let Some(oldest) = self.seen_span_order.pop_front()
        {
            self.seen_spans.remove(&oldest);
        }
        true
    }

    fn peer_connected(&mut self, record: &Record) {
        let Some(address) = record.str(protocols::peer_selection::peer::CONNECTED::FIELD_PEER) else {
            return;
        };
        let peer = self.peer_mut(address);
        peer.connected = true;
        match record.str(protocols::peer_selection::peer::CONNECTED::FIELD_DIRECTION) {
            Some("Inbound") => peer.inbound = true,
            Some("Outbound") => peer.outbound = true,
            _ => {}
        }
        peer.updated_at = Instant::now();
    }

    fn peer_disconnected(&mut self, record: &Record) {
        let Some(address) = record.str(protocols::peer_selection::peer::DISCONNECTED::FIELD_PEER) else {
            return;
        };
        if let Some(peer) = self.peers.get_mut(address) {
            peer.connected = false;
            peer.updated_at = Instant::now();
        }
    }

    fn peer_round_trip(&mut self, record: &Record) {
        let Some(address) = record.str(protocols::keepalive::peer::ROUND_TRIP::FIELD_PEER) else {
            return;
        };
        let Some(rtt_micros) = record.u64(protocols::keepalive::peer::ROUND_TRIP::FIELD_ROUND_TRIP_MICROS) else {
            return;
        };
        let peer = self.peer_mut(address);
        peer.connected = true;
        peer.outbound = true;
        peer.rtt_micros = Some(rtt_micros);
        peer.updated_at = Instant::now();
    }

    fn peer_mut(&mut self, address: &str) -> &mut Peer {
        if !self.peers.contains_key(address)
            && self.peers.len() >= PEER_STATE_LIMIT
            && let Some(oldest) =
                self.peers.values().min_by_key(|peer| peer.updated_at).map(|peer| peer.address.clone())
        {
            self.peers.remove(&oldest);
        }
        self.peers.entry(address.to_owned()).or_insert_with(|| Peer::new(address.to_owned()))
    }

    fn trim_peers(&mut self) {
        let now = Instant::now();
        self.peers
            .retain(|_, peer| peer.connected || now.saturating_duration_since(peer.updated_at) <= PEER_IDLE_LIMIT);
    }

    fn refresh_chain_quality(&mut self) {
        let now = SystemTime::now();
        self.recent_rollbacks.retain(|(at, _)| now.duration_since(*at).is_ok_and(|elapsed| elapsed <= ROLLBACK_WINDOW));
        if self.recent_rollbacks.is_empty() {
            self.chain_quality =
                ChainQuality { average_rollback_length: Some(0.0), rollback_frequency_per_second: Some(0.0) };
            return;
        }
        let total = self.recent_rollbacks.iter().map(|(_, length)| *length as f64).sum::<f64>();
        self.chain_quality.average_rollback_length = Some(total / self.recent_rollbacks.len() as f64);
        self.chain_quality.rollback_frequency_per_second =
            Some(self.recent_rollbacks.len() as f64 / ROLLBACK_WINDOW.as_secs_f64());
    }
}

fn tip(record: &Record) -> Option<Tip> {
    Some(Tip {
        header_hash: record.str(ledger::tip::UPDATE::FIELD_HEADER_HASH)?.to_owned(),
        slot: record.u64(ledger::tip::UPDATE::FIELD_SLOT)?,
        block_height: record.u64(ledger::tip::UPDATE::FIELD_BLOCK_HEIGHT)?,
        epoch: record.u64(ledger::tip::UPDATE::FIELD_EPOCH)?,
        slot_in_epoch: record.u64(ledger::tip::UPDATE::FIELD_SLOT_IN_EPOCH)?,
        density: record.f64(ledger::tip::UPDATE::FIELD_DENSITY)?,
        tx_count: record.u64(ledger::tip::UPDATE::FIELD_TX_COUNT)?,
    })
}

#[derive(Debug, Default)]
struct RateCounters {
    blocks: Rate,
    transactions: Rate,
}

impl RateCounters {
    fn sample(&mut self) {
        self.blocks.sample();
        self.transactions.sample();
    }

    fn values(&self) -> Throughput {
        Throughput {
            blocks: self.blocks.total,
            blocks_per_second: self.blocks.per_second.value.unwrap_or_default(),
            transactions: self.transactions.total,
            transactions_per_second: self.transactions.per_second.value.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Default)]
struct Rate {
    total: u64,
    last_recorded_at: Option<SystemTime>,
    last_sampled_at: Option<Instant>,
    per_second: Mean,
}

impl Rate {
    fn record(&mut self, count: u64, at: SystemTime) {
        self.total = self.total.saturating_add(count);
        let Some(previous) = self.last_recorded_at.replace(at) else {
            return;
        };
        let Ok(elapsed) = at.duration_since(previous) else {
            return;
        };
        if !elapsed.is_zero() {
            self.per_second.record_value(count as f64 / elapsed.as_secs_f64(), RATE_SMOOTHING);
        }
    }

    fn sample(&mut self) {
        let now = Instant::now();
        let Some(sampled_at) = self.last_sampled_at.replace(now) else {
            return;
        };
        if now.saturating_duration_since(sampled_at) >= Duration::from_secs(1) {
            self.per_second.record_value(0.0, RATE_SMOOTHING);
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Mean {
    value: Option<f64>,
}

impl Mean {
    fn record_value(&mut self, sample: f64, smoothing: usize) {
        let alpha = 2.0 / (smoothing.max(1) as f64 + 1.0);
        self.value = Some(match self.value {
            Some(value) => alpha * sample + (1.0 - alpha) * value,
            None => sample,
        });
    }

    fn value(&self) -> Option<u64> {
        self.value.map(|value| value.round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use amaru_observability::amaru::ledger;

    use super::*;

    fn record(target: &str, name: &str, fields: &str) -> Record {
        Record::parse(&format!(r#"{{"target":"{target}","fields":{{"message":"{name}",{fields}}}}}"#,)).expect("record")
    }

    fn tip_record(block_height: u64, tx_count: u64) -> Record {
        record(
            ledger::tip::UPDATE::TARGET,
            ledger::tip::UPDATE::NAME,
            &format!(
                r#""slot":100,"header_hash":"abc-{block_height}","block_height":{block_height},"tx_count":{tx_count},"epoch":1,"slot_in_epoch":10,"density":0.05,"current_kes_period":1,"remaining_kes_periods":2"#,
            ),
        )
    }

    #[test]
    fn reconstructs_the_tip_from_typed_schema_fields() {
        let mut projection = Projection::new("preview".into(), 1, "amaru 0.0.0".into());
        projection.apply(tip_record(4, 2), UNIX_EPOCH);

        let tip = projection.tip().expect("tip");
        assert_eq!(tip.slot, 100);
        assert_eq!(tip.header_hash, "abc-4");
    }

    #[test]
    fn derives_throughput_from_tip_deltas() {
        let mut projection = Projection::new("preview".into(), 1, "amaru 0.0.0".into());
        let start = UNIX_EPOCH + Duration::from_secs(100);

        projection.apply(tip_record(4, 2), start);
        projection.apply(tip_record(6, 5), start + Duration::from_secs(2));
        projection.apply(tip_record(7, 3), start + Duration::from_secs(3));

        let throughput = projection.throughput();
        assert_eq!(throughput.blocks, 3);
        assert_eq!(throughput.transactions, 8);
        assert_eq!(throughput.blocks_per_second, 1.0);
        assert_eq!(throughput.transactions_per_second, 3.0);
    }

    #[test]
    fn does_not_count_the_rewind_before_a_fork_is_replayed() {
        let mut projection = Projection::new("preview".into(), 1, "amaru 0.0.0".into());
        let start = UNIX_EPOCH + Duration::from_secs(100);

        projection.apply(tip_record(10, 3), start);
        projection.apply(tip_record(11, 4), start + Duration::from_secs(1));
        projection.apply(tip_record(9, 7), start + Duration::from_secs(2));

        let throughput = projection.throughput();
        assert_eq!(throughput.blocks, 1);
        assert_eq!(throughput.transactions, 4);
    }

    #[test]
    fn replays_recent_rollbacks() {
        let mut projection = Projection::new("preview".into(), 1, "amaru 0.0.0".into());
        let now = SystemTime::now();
        projection.apply(
            record(
                ledger::state::SWITCH_TO_FORK::TARGET,
                ledger::state::SWITCH_TO_FORK::NAME,
                r#""fork_point":"origin","fork_length":3,"rollback_length":2"#,
            ),
            now,
        );
        projection.set_system_sample(None);

        let quality = projection.chain_quality();
        assert_eq!(quality.average_rollback_length, Some(2.0));
        assert_eq!(quality.rollback_frequency_per_second, Some(1.0 / ROLLBACK_WINDOW.as_secs_f64()));
    }
}
