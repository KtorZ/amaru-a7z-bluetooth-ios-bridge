/**
 * The mobile side of the versioned BLE contract. Positional CBOR keeps the
 * snapshots small enough for a constrained radio link, so every shape check is
 * deliberately strict rather than trying to infer a compatible layout.
 */

/** Identifies Amaru advertisements without relying on a mutable Bluetooth name. */
export const SERVICE_UUID = "8b4cb36a-7a5d-4f9f-8f31-6a5f4fc8c711";
/** Separates read-only telemetry notifications from the optional control write. */
export const STREAM_UUID = "8b4cb36a-7a5d-4f9f-8f31-6a5f4fc8c712";
/** Keeps the power-off capability independently advertisable and removable. */
export const POWER_OFF_UUID = "8b4cb36a-7a5d-4f9f-8f31-6a5f4fc8c713";
/**
 * Names the only accepted control action. It is a framing guard, not an
 * authentication secret.
 */
export const POWER_OFF_COMMAND = new TextEncoder().encode("amaru/power-off/v1");

const MAGIC = 0xa7;
const VERSION = 5;
const HEADER_LENGTH = 8;

/**
 * One host sample. Values remain separate from trace-derived state because
 * they are observed locally by the bridge, not inferred from Amaru events.
 */
export type Resource = {
  cpuPercent: number;
  processMemoryBytes: number;
  rssBytes: number;
  virtualBytes: number;
  hostMemoryUsedBytes: number;
  hostMemoryTotalBytes: number;
  processDiskReadBytes: number;
  processDiskWriteBytes: number;
  hostDiskReadBytes: number;
  hostDiskWriteBytes: number;
  averageTemperatureCelsius: number | null;
  maximumTemperatureCelsius: number | null;
};

/**
 * The smallest peer view useful for nearby diagnosis; exposing more fields
 * would consume the fixed radio budget and disclose more network detail.
 */
export type Peer = {
  address: string;
  connected: boolean;
  inbound: boolean;
  outbound: boolean;
  rttMicros: number | null;
};

/**
 * A complete dashboard state, replacing rather than patching the previous
 * state so packet loss cannot leave the UI internally inconsistent.
 */
export type Snapshot = {
  sequence: number;
  generatedAtMillis: number;
  node: {
    network: string;
    pid: number;
    version: string;
    uptimeSeconds: number | null;
  };
  resource: Resource | null;
  throughput: {
    blocks: number;
    blocksPerSecond: number;
    transactions: number;
    transactionsPerSecond: number;
  };
  tip: {
    headerHash: string;
    slot: number;
    blockHeight: number;
    epoch: number;
    slotInEpoch: number;
    density: number;
    transactionCount: number;
  } | null;
  chainQuality: {
    averageRollbackLength: number | null;
    rollbackFrequencyPerSecond: number | null;
  };
  mempool: {
    transactions: number;
    sizeBytes: number;
  };
  peers: Peer[];
  powerOffEnabled: boolean;
};

type Frame = {
  sequence: number;
  index: number;
  count: number;
  payload: Uint8Array;
};

type Pending = {
  sequence: number;
  count: number;
  fragments: Map<number, Uint8Array>;
};

/**
 * Reassembles one notification sequence before CBOR decoding. Keeping only one
 * pending sequence bounds memory and lets a newer snapshot supersede a delayed
 * or incomplete older one.
 */
export class SnapshotStream {
  #pending: Pending | null = null;

  /** Drops incomplete fragments when a BLE connection is replaced or reset. */
  reset(): void {
    this.#pending = null;
  }

  /**
   * Accepts one BLE notification and yields a snapshot only when every fragment
   * of the latest sequence has arrived. Invalid frames are ignored so unrelated
   * advertisements cannot disrupt the dashboard.
   */
  push(notification: number[], decode: (payload: Uint8Array) => Snapshot): Snapshot | null {
    const frame = parseFrame(notification);
    if (frame === null) {
      return null;
    }

    if (this.#pending === null || this.#pending.sequence !== frame.sequence) {
      this.#pending = { sequence: frame.sequence, count: frame.count, fragments: new Map() };
    }

    if (frame.count !== this.#pending.count || frame.index >= frame.count) {
      this.#pending = null;
      return null;
    }

    this.#pending.fragments.set(frame.index, frame.payload);
    if (this.#pending.fragments.size !== this.#pending.count) {
      return null;
    }

    const payload = concatenate(this.#pending.fragments, this.#pending.count);
    this.#pending = null;
    return decode(payload);
  }
}

/**
 * Decodes the versioned positional schema emitted by the bridge. Rejecting a
 * version mismatch prevents silently assigning fields to the wrong meaning.
 */
export function decodeSnapshot(value: unknown): Snapshot {
  const snapshot = array(value, "snapshot", 11);
  const version = number(snapshot[0], "version");
  if (version !== VERSION) {
    throw new Error("Unsupported telemetry stream version");
  }

  return {
    sequence: number(snapshot[1], "sequence"),
    generatedAtMillis: number(snapshot[2], "generated_at_millis"),
    node: decodeNode(snapshot[3]),
    resource: snapshot[4] === null ? null : decodeResource(snapshot[4]),
    throughput: decodeThroughput(snapshot[5]),
    tip: snapshot[6] === null ? null : decodeTip(snapshot[6]),
    chainQuality: decodeChainQuality(snapshot[7]),
    mempool: decodeMempool(snapshot[8]),
    peers: array(snapshot[9], "peers").map(decodePeer),
    powerOffEnabled: boolean(snapshot[10], "power_off_enabled"),
  };
}

/** Rejects notifications that cannot belong to this stream before allocating reassembly state. */
function parseFrame(notification: number[]): Frame | null {
  const bytes = Uint8Array.from(notification);
  if (
    bytes.length <= HEADER_LENGTH ||
    bytes[0] !== MAGIC ||
    bytes[1] !== VERSION
  ) {
    return null;
  }

  return {
    sequence: new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(2),
    index: bytes[6],
    count: bytes[7],
    payload: bytes.slice(HEADER_LENGTH),
  };
}

/** Restores wire order explicitly because BLE notifications need not arrive in fragment order. */
function concatenate(fragments: Map<number, Uint8Array>, count: number): Uint8Array {
  const parts = [] as Uint8Array[];
  for (let index = 0; index < count; index += 1) {
    const part = fragments.get(index);
    if (part === undefined) throw new Error("Incomplete telemetry snapshot");
    parts.push(part);
  }

  const size = parts.reduce((total, part) => total + part.length, 0);
  const payload = new Uint8Array(size);
  let offset = 0;
  for (const part of parts) {
    payload.set(part, offset);
    offset += part.length;
  }
  return payload;
}

function decodeNode(value: unknown): Snapshot["node"] {
  const node = array(value, "node", 3, 4);
  return {
    network: string(node[0], "node.network"),
    pid: number(node[1], "node.pid"),
    version: string(node[2], "node.version"),
    uptimeSeconds: optionalNumber(node[3], "node.uptime_seconds"),
  };
}

function decodeResource(value: unknown): Resource {
  const resource = array(value, "resource", 10, 12);
  return {
    cpuPercent: number(resource[0], "resource.cpu_percent"),
    processMemoryBytes: number(resource[1], "resource.process_memory_bytes"),
    rssBytes: number(resource[2], "resource.rss_bytes"),
    virtualBytes: number(resource[3], "resource.virtual_bytes"),
    hostMemoryUsedBytes: number(resource[4], "resource.host_memory_used_bytes"),
    hostMemoryTotalBytes: number(resource[5], "resource.host_memory_total_bytes"),
    processDiskReadBytes: number(resource[6], "resource.process_disk_read_bytes"),
    processDiskWriteBytes: number(resource[7], "resource.process_disk_write_bytes"),
    hostDiskReadBytes: number(resource[8], "resource.host_disk_read_bytes"),
    hostDiskWriteBytes: number(resource[9], "resource.host_disk_write_bytes"),
    averageTemperatureCelsius: optionalNumber(resource[10], "resource.average_temperature_celsius"),
    maximumTemperatureCelsius: optionalNumber(resource[11], "resource.maximum_temperature_celsius"),
  };
}

function decodeThroughput(value: unknown): Snapshot["throughput"] {
  const throughput = array(value, "throughput", 4);
  return {
    blocks: number(throughput[0], "throughput.blocks"),
    blocksPerSecond: number(throughput[1], "throughput.blocks_per_second"),
    transactions: number(throughput[2], "throughput.transactions"),
    transactionsPerSecond: number(throughput[3], "throughput.transactions_per_second"),
  };
}

function decodeTip(value: unknown): NonNullable<Snapshot["tip"]> {
  const tip = array(value, "tip", 7);
  return {
    headerHash: string(tip[0], "tip.header_hash"),
    slot: number(tip[1], "tip.slot"),
    blockHeight: number(tip[2], "tip.block_height"),
    epoch: number(tip[3], "tip.epoch"),
    slotInEpoch: number(tip[4], "tip.slot_in_epoch"),
    density: number(tip[5], "tip.density"),
    transactionCount: number(tip[6], "tip.transaction_count"),
  };
}

function decodeChainQuality(value: unknown): Snapshot["chainQuality"] {
  const quality = array(value, "chain_quality", 0, 2);
  return {
    averageRollbackLength: optionalNumber(quality[0], "chain_quality.average_rollback_length"),
    rollbackFrequencyPerSecond: optionalNumber(quality[1], "chain_quality.rollback_frequency_per_second"),
  };
}

function decodeMempool(value: unknown): Snapshot["mempool"] {
  const mempool = array(value, "mempool", 2);
  return {
    transactions: number(mempool[0], "mempool.transactions"),
    sizeBytes: number(mempool[1], "mempool.size_bytes"),
  };
}

function decodePeer(value: unknown): Peer {
  const peer = array(value, "peer", 4);
  const direction = number(peer[3], "peer.direction");
  if (!Number.isInteger(direction) || direction < 0 || direction > 3) {
    throw new Error("Invalid peer.direction");
  }

  return {
    address: string(peer[0], "peer.address"),
    connected: boolean(peer[1], "peer.connected"),
    rttMicros: optionalNumber(peer[2], "peer.rtt_micros"),
    outbound: (direction & 1) !== 0,
    inbound: (direction & 2) !== 0,
  };
}

/** Accepts omitted trailing optional fields because minicbor elides them to save radio bytes. */
function array(value: unknown, name: string, minimumLength?: number, maximumLength = minimumLength): unknown[] {
  if (
    !Array.isArray(value) ||
    (minimumLength !== undefined && (value.length < minimumLength || value.length > maximumLength!))
  ) {
    throw new Error(`Invalid ${name} payload`);
  }
  return value;
}

function string(value: unknown, name: string): string {
  if (typeof value !== "string") {
    throw new Error(`Invalid ${name}`);
  }
  return value;
}

function number(value: unknown, name: string): number {
  if (typeof value === "bigint") {
    const converted = Number(value);
    if (Number.isSafeInteger(converted)) {
      return converted;
    }
  } else if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }

  throw new Error(`Invalid ${name}`);
}

function optionalNumber(value: unknown, name: string): number | null {
  return value === null || value === undefined ? null : number(value, name);
}

function boolean(value: unknown, name: string): boolean {
  if (typeof value !== "boolean") {
    throw new Error(`Invalid ${name}`);
  }
  return value;
}
