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

//! Versioned wire format for the mobile telemetry characteristic.

use std::time::{SystemTime, UNIX_EPOCH};

use minicbor::{Encode, Encoder};

use crate::projection::{ChainQuality, Mempool, Peer, Projection, ResourceSample, Throughput, Tip};

/// Current CBOR snapshot version.
///
/// Positional arrays have no self-describing field names, so any semantic
/// layout change must advance this value and be deployed with the mobile app.
pub const SNAPSHOT_VERSION: u8 = 5;

/// One-byte marker used to reject unrelated GATT notifications quickly.
pub const FRAGMENT_MAGIC: u8 = 0xa7;

/// Header length for each BLE notification fragment, fixed to keep reassembly allocation-free.
pub const FRAGMENT_HEADER_LEN: usize = 8;

/// Payload small enough for conservative iOS BLE MTUs after the fragment header.
pub const FRAGMENT_PAYLOAD_LEN: usize = 160;

/// The bridge never emits more than this payload per second, reserving budget for fragment headers.
pub const MAX_SNAPSHOT_BYTES: usize = 7 * 1_024;

/// Encodes the latest complete state as a CBOR array.
///
/// A CBOR array keeps the client decoder compact. Its field order and shape are part of the
/// versioned contract; changing either requires a new [`SNAPSHOT_VERSION`].
/// Peers are removed from the least responsive end only when necessary to
/// preserve the radio budget, leaving the app with a valid whole snapshot.
#[allow(clippy::expect_used)]
pub fn snapshot_bytes(projection: &Projection, sequence: u32, power_off_enabled: bool) -> Vec<u8> {
    let mut peers = projection.peers();
    loop {
        let snapshot = Snapshot::from_projection(projection, sequence, &peers, power_off_enabled);
        let mut bytes = Vec::with_capacity(2_048);
        Encoder::new(&mut bytes).encode(snapshot).expect("encoding mobile telemetry cannot fail");
        if bytes.len() <= MAX_SNAPSHOT_BYTES || peers.is_empty() {
            return bytes;
        }
        peers.pop();
    }
}

/// Splits one CBOR snapshot into independently transportable GATT notifications.
///
/// Every fragment repeats its sequence and expected count so the receiver can
/// discard an incomplete old snapshot as soon as a newer one begins.
#[allow(clippy::expect_used)]
pub fn fragment(sequence: u32, snapshot: &[u8]) -> Vec<Vec<u8>> {
    let chunks = snapshot.chunks(FRAGMENT_PAYLOAD_LEN).collect::<Vec<_>>();
    let chunk_count = u8::try_from(chunks.len()).expect("snapshot fragment count must fit in u8");

    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let mut frame = Vec::with_capacity(FRAGMENT_HEADER_LEN + chunk.len());
            frame.push(FRAGMENT_MAGIC);
            frame.push(SNAPSHOT_VERSION);
            frame.extend_from_slice(&sequence.to_be_bytes());
            frame.push(u8::try_from(index).expect("snapshot fragment index must fit in u8"));
            frame.push(chunk_count);
            frame.extend_from_slice(chunk);
            frame
        })
        .collect()
}

#[derive(Encode)]
#[cbor(array)]
struct Snapshot<'a> {
    #[n(0)]
    version: u8,
    #[n(1)]
    sequence: u32,
    #[n(2)]
    generated_at_millis: u64,
    #[n(3)]
    node: Node<'a>,
    #[n(4)]
    resource: Option<Resource>,
    #[n(5)]
    throughput: ThroughputWire,
    #[n(6)]
    tip: Option<TipWire<'a>>,
    #[n(7)]
    chain_quality: ChainQualityWire,
    #[n(8)]
    mempool: MempoolWire,
    #[n(9)]
    peers: Vec<PeerWire<'a>>,
    #[n(10)]
    power_off_enabled: bool,
}

impl<'a> Snapshot<'a> {
    fn from_projection(projection: &'a Projection, sequence: u32, peers: &'a [Peer], power_off_enabled: bool) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            sequence,
            generated_at_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            node: Node::from_projection(projection),
            resource: projection.system_sample().map(Resource::from_sample),
            throughput: ThroughputWire::from_throughput(&projection.throughput()),
            tip: projection.tip().map(TipWire::from_tip),
            chain_quality: ChainQualityWire::from_chain_quality(projection.chain_quality()),
            mempool: MempoolWire::from_mempool(projection.mempool()),
            peers: peers.iter().map(PeerWire::from_peer).collect(),
            power_off_enabled,
        }
    }
}

#[derive(Encode)]
#[cbor(array)]
struct Node<'a> {
    #[n(0)]
    network: &'a str,
    #[n(1)]
    pid: u32,
    #[n(2)]
    version: &'a str,
    #[n(3)]
    uptime_seconds: Option<u64>,
}

impl<'a> Node<'a> {
    fn from_projection(projection: &'a Projection) -> Self {
        Self {
            network: projection.network(),
            pid: projection.pid(),
            version: projection.version(),
            uptime_seconds: projection.system_sample().map(|sample| sample.runtime_seconds),
        }
    }
}

#[derive(Encode)]
#[cbor(array)]
struct Resource {
    #[n(0)]
    cpu_percent: f64,
    #[n(1)]
    process_memory_bytes: u64,
    #[n(2)]
    rss_bytes: u64,
    #[n(3)]
    virtual_bytes: u64,
    #[n(4)]
    host_memory_used_bytes: u64,
    #[n(5)]
    host_memory_total_bytes: u64,
    #[n(6)]
    process_disk_read_bytes: u64,
    #[n(7)]
    process_disk_write_bytes: u64,
    #[n(8)]
    host_disk_read_bytes: u64,
    #[n(9)]
    host_disk_write_bytes: u64,
    #[n(10)]
    average_temperature_celsius: Option<f32>,
    #[n(11)]
    maximum_temperature_celsius: Option<f32>,
}

impl Resource {
    fn from_sample(sample: &ResourceSample) -> Self {
        Self {
            cpu_percent: sample.cpu_percent,
            process_memory_bytes: sample.process_memory_bytes,
            rss_bytes: sample.rss_bytes,
            virtual_bytes: sample.virtual_bytes,
            host_memory_used_bytes: sample.host_memory_used_bytes,
            host_memory_total_bytes: sample.host_memory_total_bytes,
            process_disk_read_bytes: sample.process_disk_read_bytes,
            process_disk_write_bytes: sample.process_disk_write_bytes,
            host_disk_read_bytes: sample.host_disk_read_bytes,
            host_disk_write_bytes: sample.host_disk_write_bytes,
            average_temperature_celsius: sample.average_temperature_celsius,
            maximum_temperature_celsius: sample.maximum_temperature_celsius,
        }
    }
}

#[derive(Encode)]
#[cbor(array)]
struct ThroughputWire {
    #[n(0)]
    blocks: u64,
    #[n(1)]
    blocks_per_second: f64,
    #[n(2)]
    transactions: u64,
    #[n(3)]
    transactions_per_second: f64,
}

impl ThroughputWire {
    fn from_throughput(throughput: &Throughput) -> Self {
        Self {
            blocks: throughput.blocks,
            blocks_per_second: throughput.blocks_per_second,
            transactions: throughput.transactions,
            transactions_per_second: throughput.transactions_per_second,
        }
    }
}

#[derive(Encode)]
#[cbor(array)]
struct TipWire<'a> {
    #[n(0)]
    header_hash: &'a str,
    #[n(1)]
    slot: u64,
    #[n(2)]
    block_height: u64,
    #[n(3)]
    epoch: u64,
    #[n(4)]
    slot_in_epoch: u64,
    #[n(5)]
    density: f64,
    #[n(6)]
    tx_count: u64,
}

impl<'a> TipWire<'a> {
    fn from_tip(tip: &'a Tip) -> Self {
        Self {
            header_hash: &tip.header_hash,
            slot: tip.slot,
            block_height: tip.block_height,
            epoch: tip.epoch,
            slot_in_epoch: tip.slot_in_epoch,
            density: tip.density,
            tx_count: tip.tx_count,
        }
    }
}

#[derive(Encode)]
#[cbor(array)]
struct ChainQualityWire {
    #[n(0)]
    average_rollback_length: Option<f64>,
    #[n(1)]
    rollback_frequency_per_second: Option<f64>,
}

impl ChainQualityWire {
    fn from_chain_quality(chain_quality: &ChainQuality) -> Self {
        Self {
            average_rollback_length: chain_quality.average_rollback_length,
            rollback_frequency_per_second: chain_quality.rollback_frequency_per_second,
        }
    }
}

#[derive(Encode)]
#[cbor(array)]
struct MempoolWire {
    #[n(0)]
    transactions: u64,
    #[n(1)]
    size_bytes: u64,
}

impl MempoolWire {
    fn from_mempool(mempool: &Mempool) -> Self {
        Self { transactions: mempool.transactions, size_bytes: mempool.size_bytes }
    }
}

#[derive(Encode)]
#[cbor(array)]
struct PeerWire<'a> {
    #[n(0)]
    address: &'a str,
    #[n(1)]
    connected: bool,
    #[n(2)]
    rtt_micros: Option<u64>,
    #[n(3)]
    direction: u8,
}

impl<'a> PeerWire<'a> {
    fn from_peer(peer: &'a Peer) -> Self {
        Self {
            address: &peer.address,
            connected: peer.connected,
            rtt_micros: peer.rtt_micros,
            direction: u8::from(peer.outbound) | (u8::from(peer.inbound) << 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use minicbor::Decode;

    use super::*;
    use crate::projection::{Peer, Projection};

    #[test]
    fn wire_output_stays_under_the_per_second_budget() {
        let mut projection = Projection::new("mainnet".into(), 42, "amaru 0.0.0".into());
        for index in 0..512 {
            projection.insert_peer(Peer::new(format!("[2001:db8::{index}]:3001")));
        }

        let bytes = snapshot_bytes(&projection, 7, false);
        assert!(bytes.len() <= MAX_SNAPSHOT_BYTES);

        let fragments = fragment(7, &bytes);
        assert!(fragments.iter().all(|frame| frame.len() <= FRAGMENT_HEADER_LEN + FRAGMENT_PAYLOAD_LEN));
        assert!(fragments.iter().map(Vec::len).sum::<usize>() <= 10 * 1_024);
    }

    #[test]
    fn golden_snapshots_are_emitted_and_decode_in_rust() {
        let full: DecodedSnapshot =
            minicbor::decode(&write_fixture("snapshot-v5-full.cbor", full_snapshot())).expect("decode full fixture");
        assert_eq!(full.version, SNAPSHOT_VERSION);
        assert_eq!(full.sequence, 4_294_967_000);
        assert_eq!(full.generated_at_millis, 1_788_480_000_123);
        assert_eq!(full.node.network, "mainnet");
        assert_eq!(full.node.pid, 42_424);
        assert_eq!(full.node.uptime_seconds, Some(86_400));
        assert_eq!(full.resource.expect("resource").maximum_temperature_celsius, Some(87.5));
        assert_eq!(
            full.tip.expect("tip").header_hash,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(full.peers.len(), 1);
        assert_eq!(full.peers[0].direction, 3);

        let without_temperatures: DecodedSnapshot =
            minicbor::decode(&write_fixture("snapshot-v5-without-temperatures.cbor", without_temperatures_snapshot()))
                .expect("decode fixture without temperatures");
        let resource = without_temperatures.resource.expect("resource");
        assert_eq!(resource.average_temperature_celsius, None);
        assert_eq!(resource.maximum_temperature_celsius, None);

        let empty: DecodedSnapshot =
            minicbor::decode(&write_fixture("snapshot-v5-empty.cbor", empty_snapshot())).expect("decode empty fixture");
        assert!(empty.resource.is_none());
        assert!(empty.tip.is_none());
        assert!(empty.peers.is_empty());
    }

    fn write_fixture(name: &str, snapshot: Snapshot<'_>) -> Vec<u8> {
        let mut bytes = Vec::new();
        Encoder::new(&mut bytes).encode(snapshot).expect("encode fixture");

        let directory = fixture_directory();
        fs::create_dir_all(&directory).expect("create fixture directory");
        fs::write(directory.join(name), &bytes).expect("write fixture");
        bytes
    }

    fn fixture_directory() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-vectors")
    }

    fn full_snapshot() -> Snapshot<'static> {
        Snapshot {
            version: SNAPSHOT_VERSION,
            sequence: 4_294_967_000,
            generated_at_millis: 1_788_480_000_123,
            node: Node {
                network: "mainnet",
                pid: 42_424,
                version: "amaru 10.11.0 (b693f81)",
                uptime_seconds: Some(86_400),
            },
            resource: Some(Resource {
                cpu_percent: 76.5,
                process_memory_bytes: 987_654_321,
                rss_bytes: 1_234_567_890,
                virtual_bytes: 2_345_678_901,
                host_memory_used_bytes: 3_456_789_012,
                host_memory_total_bytes: 4_567_890_123,
                process_disk_read_bytes: 5_678_901,
                process_disk_write_bytes: 6_789_012,
                host_disk_read_bytes: 7_890_123,
                host_disk_write_bytes: 8_901_234,
                average_temperature_celsius: Some(42.5),
                maximum_temperature_celsius: Some(87.5),
            }),
            throughput: ThroughputWire {
                blocks: 123_456,
                blocks_per_second: 78.9,
                transactions: 987_654,
                transactions_per_second: 12.3,
            },
            tip: Some(TipWire {
                header_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                slot: 123_456_789,
                block_height: 9_876_543,
                epoch: 612,
                slot_in_epoch: 345_678,
                density: 0.05,
                tx_count: 42,
            }),
            chain_quality: ChainQualityWire {
                average_rollback_length: Some(1.75),
                rollback_frequency_per_second: Some(0.0125),
            },
            mempool: MempoolWire { transactions: 37, size_bytes: 9_876 },
            peers: vec![PeerWire {
                address: "relay.example:3001",
                connected: true,
                rtt_micros: Some(1_234),
                direction: 3,
            }],
            power_off_enabled: true,
        }
    }

    fn without_temperatures_snapshot() -> Snapshot<'static> {
        Snapshot {
            version: SNAPSHOT_VERSION,
            sequence: 7,
            generated_at_millis: 1_788_480_001_234,
            node: Node { network: "preview", pid: 7, version: "amaru 10.11.0", uptime_seconds: None },
            resource: Some(Resource {
                cpu_percent: 0.0,
                process_memory_bytes: 0,
                rss_bytes: 0,
                virtual_bytes: 0,
                host_memory_used_bytes: 0,
                host_memory_total_bytes: 0,
                process_disk_read_bytes: 0,
                process_disk_write_bytes: 0,
                host_disk_read_bytes: 0,
                host_disk_write_bytes: 0,
                average_temperature_celsius: None,
                maximum_temperature_celsius: None,
            }),
            throughput: ThroughputWire {
                blocks: 0,
                blocks_per_second: 0.0,
                transactions: 0,
                transactions_per_second: 0.0,
            },
            tip: None,
            chain_quality: ChainQualityWire { average_rollback_length: None, rollback_frequency_per_second: None },
            mempool: MempoolWire { transactions: 0, size_bytes: 0 },
            peers: Vec::new(),
            power_off_enabled: false,
        }
    }

    fn empty_snapshot() -> Snapshot<'static> {
        Snapshot {
            version: SNAPSHOT_VERSION,
            sequence: 0,
            generated_at_millis: 0,
            node: Node { network: "preprod", pid: 0, version: "amaru", uptime_seconds: None },
            resource: None,
            throughput: ThroughputWire {
                blocks: 0,
                blocks_per_second: 0.0,
                transactions: 0,
                transactions_per_second: 0.0,
            },
            tip: None,
            chain_quality: ChainQualityWire { average_rollback_length: None, rollback_frequency_per_second: None },
            mempool: MempoolWire { transactions: 0, size_bytes: 0 },
            peers: Vec::new(),
            power_off_enabled: false,
        }
    }

    #[derive(Decode)]
    #[cbor(array)]
    struct DecodedSnapshot {
        #[n(0)]
        version: u8,
        #[n(1)]
        sequence: u32,
        #[n(2)]
        generated_at_millis: u64,
        #[n(3)]
        node: DecodedNode,
        #[n(4)]
        resource: Option<DecodedResource>,
        #[n(5)]
        _throughput: DecodedThroughput,
        #[n(6)]
        tip: Option<DecodedTip>,
        #[n(7)]
        _chain_quality: DecodedChainQuality,
        #[n(8)]
        _mempool: DecodedMempool,
        #[n(9)]
        peers: Vec<DecodedPeer>,
        #[n(10)]
        _power_off_enabled: bool,
    }

    #[derive(Decode)]
    #[cbor(array)]
    struct DecodedNode {
        #[n(0)]
        network: String,
        #[n(1)]
        pid: u32,
        #[n(2)]
        _version: String,
        #[n(3)]
        uptime_seconds: Option<u64>,
    }

    #[derive(Decode)]
    #[cbor(array)]
    struct DecodedResource {
        #[n(0)]
        _cpu_percent: f64,
        #[n(1)]
        _process_memory_bytes: u64,
        #[n(2)]
        _rss_bytes: u64,
        #[n(3)]
        _virtual_bytes: u64,
        #[n(4)]
        _host_memory_used_bytes: u64,
        #[n(5)]
        _host_memory_total_bytes: u64,
        #[n(6)]
        _process_disk_read_bytes: u64,
        #[n(7)]
        _process_disk_write_bytes: u64,
        #[n(8)]
        _host_disk_read_bytes: u64,
        #[n(9)]
        _host_disk_write_bytes: u64,
        #[n(10)]
        average_temperature_celsius: Option<f32>,
        #[n(11)]
        maximum_temperature_celsius: Option<f32>,
    }

    #[derive(Decode)]
    #[cbor(array)]
    struct DecodedThroughput {
        #[n(0)]
        _blocks: u64,
        #[n(1)]
        _blocks_per_second: f64,
        #[n(2)]
        _transactions: u64,
        #[n(3)]
        _transactions_per_second: f64,
    }

    #[derive(Decode)]
    #[cbor(array)]
    struct DecodedTip {
        #[n(0)]
        header_hash: String,
        #[n(1)]
        _slot: u64,
        #[n(2)]
        _block_height: u64,
        #[n(3)]
        _epoch: u64,
        #[n(4)]
        _slot_in_epoch: u64,
        #[n(5)]
        _density: f64,
        #[n(6)]
        _tx_count: u64,
    }

    #[derive(Decode)]
    #[cbor(array)]
    struct DecodedChainQuality {
        #[n(0)]
        _average_rollback_length: Option<f64>,
        #[n(1)]
        _rollback_frequency_per_second: Option<f64>,
    }

    #[derive(Decode)]
    #[cbor(array)]
    struct DecodedMempool {
        #[n(0)]
        _transactions: u64,
        #[n(1)]
        _size_bytes: u64,
    }

    #[derive(Decode)]
    #[cbor(array)]
    struct DecodedPeer {
        #[n(0)]
        _address: String,
        #[n(1)]
        _connected: bool,
        #[n(2)]
        _rtt_micros: Option<u64>,
        #[n(3)]
        direction: u8,
    }
}
