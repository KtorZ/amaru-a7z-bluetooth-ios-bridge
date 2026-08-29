import { decode } from "cbor-x";

import { decodeSnapshot } from "./protocol.js";

/** Checks the complete version-5 snapshot produced by the Rust bridge. */
export function assertFullSnapshot(bytes: Uint8Array): void {
  const snapshot = decodeSnapshot(decode(bytes));

  equal(snapshot.sequence, 4_294_967_000, "sequence");
  equal(snapshot.generatedAtMillis, 1_788_480_000_123, "generated_at_millis");
  equal(snapshot.node.network, "mainnet", "node.network");
  equal(snapshot.node.pid, 42_424, "node.pid");
  equal(snapshot.node.version, "amaru 10.11.0 (b693f81)", "node.version");
  equal(snapshot.node.uptimeSeconds, 86_400, "node.uptime_seconds");

  const resource = required(snapshot.resource, "resource");
  equal(resource.cpuPercent, 76.5, "resource.cpu_percent");
  equal(resource.processMemoryBytes, 987_654_321, "resource.process_memory_bytes");
  equal(resource.rssBytes, 1_234_567_890, "resource.rss_bytes");
  equal(resource.virtualBytes, 2_345_678_901, "resource.virtual_bytes");
  equal(resource.hostMemoryUsedBytes, 3_456_789_012, "resource.host_memory_used_bytes");
  equal(resource.hostMemoryTotalBytes, 4_567_890_123, "resource.host_memory_total_bytes");
  equal(resource.processDiskReadBytes, 5_678_901, "resource.process_disk_read_bytes");
  equal(resource.processDiskWriteBytes, 6_789_012, "resource.process_disk_write_bytes");
  equal(resource.hostDiskReadBytes, 7_890_123, "resource.host_disk_read_bytes");
  equal(resource.hostDiskWriteBytes, 8_901_234, "resource.host_disk_write_bytes");
  equal(resource.averageTemperatureCelsius, 42.5, "resource.average_temperature_celsius");
  equal(resource.maximumTemperatureCelsius, 87.5, "resource.maximum_temperature_celsius");

  equal(snapshot.throughput.blocks, 123_456, "throughput.blocks");
  equal(snapshot.throughput.blocksPerSecond, 78.9, "throughput.blocks_per_second");
  equal(snapshot.throughput.transactions, 987_654, "throughput.transactions");
  equal(snapshot.throughput.transactionsPerSecond, 12.3, "throughput.transactions_per_second");

  const tip = required(snapshot.tip, "tip");
  equal(tip.headerHash, "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "tip.header_hash");
  equal(tip.slot, 123_456_789, "tip.slot");
  equal(tip.blockHeight, 9_876_543, "tip.block_height");
  equal(tip.epoch, 612, "tip.epoch");
  equal(tip.slotInEpoch, 345_678, "tip.slot_in_epoch");
  equal(tip.density, 0.05, "tip.density");
  equal(tip.transactionCount, 42, "tip.tx_count");

  equal(snapshot.chainQuality.averageRollbackLength, 1.75, "chain_quality.average_rollback_length");
  equal(snapshot.chainQuality.rollbackFrequencyPerSecond, 0.0125, "chain_quality.rollback_frequency_per_second");
  equal(snapshot.mempool.transactions, 37, "mempool.transactions");
  equal(snapshot.mempool.sizeBytes, 9_876, "mempool.size_bytes");
  equal(snapshot.peers.length, 1, "peers.length");
  equal(snapshot.peers[0]?.address, "relay.example:3001", "peers[0].address");
  equal(snapshot.peers[0]?.connected, true, "peers[0].connected");
  equal(snapshot.peers[0]?.inbound, true, "peers[0].inbound");
  equal(snapshot.peers[0]?.outbound, true, "peers[0].outbound");
  equal(snapshot.peers[0]?.rttMicros, 1_234, "peers[0].rtt_micros");
  equal(snapshot.powerOffEnabled, true, "power_off_enabled");
}

/** Checks a version-5 resource whose trailing temperature fields are omitted. */
export function assertSnapshotWithoutTemperatures(bytes: Uint8Array): void {
  const snapshot = decodeSnapshot(decode(bytes));
  const resource = required(snapshot.resource, "resource");

  equal(snapshot.node.uptimeSeconds, null, "node.uptime_seconds");
  equal(resource.averageTemperatureCelsius, null, "resource.average_temperature_celsius");
  equal(resource.maximumTemperatureCelsius, null, "resource.maximum_temperature_celsius");
  equal(snapshot.tip, null, "tip");
  equal(snapshot.peers.length, 0, "peers.length");
}

/** Checks a version-5 snapshot with no resource or tip state. */
export function assertEmptySnapshot(bytes: Uint8Array): void {
  const snapshot = decodeSnapshot(decode(bytes));

  equal(snapshot.node.network, "preprod", "node.network");
  equal(snapshot.resource, null, "resource");
  equal(snapshot.tip, null, "tip");
  equal(snapshot.peers.length, 0, "peers.length");
  equal(snapshot.powerOffEnabled, false, "power_off_enabled");
}

/** Ensures the application does not silently accept an older wire format. */
export function assertPriorVersionRejected(bytes: Uint8Array): void {
  const snapshot = decode(bytes);
  if (!Array.isArray(snapshot)) throw new Error("Invalid fixture snapshot");
  snapshot[0] = 4;

  let rejected = false;
  try {
    decodeSnapshot(snapshot);
  } catch {
    rejected = true;
  }
  if (!rejected) throw new Error("Accepted a prior telemetry stream version");
}

function required<T>(value: T | null, name: string): T {
  if (value === null) throw new Error(`Missing ${name}`);
  return value;
}

function equal<T>(actual: T, expected: T, name: string): void {
  if (actual !== expected) {
    throw new Error(`Unexpected ${name}: expected ${String(expected)}, got ${String(actual)}`);
  }
}
