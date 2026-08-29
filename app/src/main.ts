import type { BleDevice } from "@mnlphlp/plugin-blec";
import { invoke } from "@tauri-apps/api/core";
import { decode } from "cbor-x";

import { bytes, count, dataRate, duration, percent, rate, uptime } from "./format";
import {
  POWER_OFF_COMMAND,
  POWER_OFF_UUID,
  SERVICE_UUID,
  STREAM_UUID,
  SnapshotStream,
  decodeSnapshot,
  type Snapshot,
} from "./protocol";
import "./style.css";

/**
 * The dashboard is a replace-on-snapshot view. It deliberately holds no
 * historical node data: connection activity is bounded local UI feedback, and
 * every displayed node value comes from the newest complete BLE snapshot.
 */

type Connection = "disconnected" | "scanning" | "connecting" | "awaiting" | "connected" | "shutting_down";
type Bluetooth = typeof import("@mnlphlp/plugin-blec");
type ActivityLevel = "info" | "success" | "error";

type Activity = {
  readonly level: ActivityLevel;
  readonly message: string;
  readonly timestamp: number;
};

const app = element("#app");
const amaruLogo = new URL("../amaru.svg", import.meta.url).href;
const hasTauriRuntime = "__TAURI_INTERNALS__" in window;
const RESET_TIMEOUT_MS = 1_000;
const SCAN_TIMEOUT_MS = 10_000;
const SIGNAL_TIMEOUT_MS = 2_000;
const POWER_OFF_ANIMATION_MS = 800;
const ACTIVITY_CAPACITY = 5;
const EPSILON = 1e-1;

const devices = new Map<string, BleDevice>();
const stream = new SnapshotStream();
let bluetooth: Bluetooth | null = null;
let connection: Connection = "disconnected";
let snapshot: Snapshot | null = null;
let lastPayloadAt: number | null = null;
let lastTipHash: string | null = null;
let lastTipUpdateAt: number | null = null;
let selectedAddress: string | null = null;
let error: string | null = null;
let initialDiscoveryPending = hasTauriRuntime;
let resuming = false;
let connectionAttempt = 0;
let powerOffPending = false;
const activities: Activity[] = [];

void initialise();
render();
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") void resume();
});
window.setInterval(() => {
  if (snapshot !== null && connection !== "shutting_down" && !powerOffPending) render();
}, 1_000);

async function initialise(): Promise<void> {
  if (!hasTauriRuntime) {
    render();
    return;
  }

  try {
    bluetooth = await import("@mnlphlp/plugin-blec");
    addActivity("Bluetooth is ready");
    await bluetooth.getScanningUpdates((scanning) => {
      if (connection === "disconnected" || connection === "scanning") {
        connection = scanning ? "scanning" : "disconnected";
        render();
      }
    });
    void discover(true);
  } catch (cause) {
    initialDiscoveryPending = false;
    error = message(cause);
    addActivity(error, "error");
    render();
  }
}

/**
 * Finds a nearby bridge without maintaining a device cache across scans. An
 * automatic scan connects to the first matching node; a manual scan leaves the
 * discovered choice visible for the operator.
 */
async function discover(connectFirst: boolean): Promise<void> {
  if (connection === "scanning" || connection === "connecting" || connection === "awaiting" || connection === "connected") {
    return;
  }

  try {
    const ble = bluetoothApi();
    error = null;
    if (!(await ble.checkPermissions(true))) {
      throw new Error("Bluetooth permission is required to find Amaru nodes");
    }
    devices.clear();
    connection = "scanning";
    addActivity("Scanning for nearby Amaru nodes");
    render();
    let foundAmaru = false;
    let resolveDiscovery = () => {};
    const discovery = new Promise<void>((resolve) => {
      resolveDiscovery = resolve;
    });
    await ble.startScan((found) => {
      if (foundAmaru) return;

      const device = found.find(isAmaru);
      if (device === undefined) return;

      foundAmaru = true;
      devices.set(device.address, device);
      initialDiscoveryPending = false;
      addActivity(`Found ${device.name || "Amaru node"}`, "success");
      resolveDiscovery();
      if (connectFirst) {
        void attach(device.address);
      } else {
        render();
        void stopScanAfterDiscovery(ble);
      }
    }, SCAN_TIMEOUT_MS);

    await Promise.race([discovery, delay(SCAN_TIMEOUT_MS)]);

    if (!foundAmaru) {
      initialDiscoveryPending = false;
      connection = "disconnected";
      addActivity("No Amaru node found nearby");
      render();
    }
  } catch (cause) {
    initialDiscoveryPending = false;
    connection = "disconnected";
    error = message(cause);
    addActivity(error, "error");
    render();
  }
}

/** Stops a manual scan once it has found a node so the BLE plugin can serve the selected connection. */
async function stopScanAfterDiscovery(ble: Bluetooth): Promise<void> {
  try {
    await ble.stopScan();
  } catch (cause) {
    if (connection !== "scanning") return;

    connection = "disconnected";
    error = message(cause);
    addActivity(error, "error");
    render();
  }
}

/**
 * Serialises connection attempts through a monotonically increasing token so a
 * delayed callback from an old attempt cannot replace a newer connection.
 */
async function attach(address: string): Promise<void> {
  if (connection === "connecting" || connection === "awaiting" || connection === "connected") return;

  let ble: Bluetooth | null = null;
  const attempt = ++connectionAttempt;

  try {
    ble = bluetoothApi();
    connection = "connecting";
    selectedAddress = address;
    stream.reset();
    error = null;
    addActivity(`Connecting to ${devices.get(address)?.name || "Amaru node"}`);
    render();

    await ble.stopScan();
    if (attempt !== connectionAttempt) return;
    await ble.connect(address, () => onDisconnect(attempt));
    if (attempt !== connectionAttempt) return;
    addActivity("Connected to Amaru", "success");
    await ble.subscribe(STREAM_UUID, SERVICE_UUID, receiveNotification);
    if (attempt !== connectionAttempt) return;
    connection = "awaiting";
    addActivity("Subscribed to live telemetry", "success");
  } catch (cause) {
    if (attempt !== connectionAttempt) return;
    if (ble !== null) await resetQuietly();
    connection = "disconnected";
    error = message(cause);
    addActivity(error, "error");
  }
  if (attempt !== connectionAttempt) return;
  render();
}

/**
 * Bounds recovery from a platform BLE operation that may otherwise wait longer
 * than the user should have to wait before retrying discovery.
 */
async function resetQuietly(): Promise<void> {
  try {
    await Promise.race([
      invoke("plugin:blec|reset_connection"),
      new Promise<void>((resolve) => window.setTimeout(resolve, RESET_TIMEOUT_MS)),
    ]);
  } catch {
    // A best-effort cleanup must never prevent a retry.
  }
}

/**
 * Rebuilds a connection after foregrounding because iOS may retain stale BLE
 * state while the application is suspended.
 */
async function resume(): Promise<void> {
  if (resuming || connection === "shutting_down" || bluetooth === null) return;

  resuming = true;
  initialDiscoveryPending = true;
  try {
    addActivity("Reconnecting to Amaru");
    connectionAttempt += 1;
    await resetQuietly();
    onDisconnect();
    await discover(true);
  } finally {
    resuming = false;
  }
}

/**
 * Plays the off transition before sending the fixed command. The dashboard
 * only enters its shutdown view once that intent has become visible.
 */
async function powerOff(): Promise<void> {
  if (powerOffPending || connection === "shutting_down") return;

  const previousConnection = connection;
  powerOffPending = true;
  render();

  try {
    await delay(POWER_OFF_ANIMATION_MS);
    if (!powerOffPending) return;

    connection = "shutting_down";
    powerOffPending = false;
    error = null;
    render();
    addActivity("Power-off request sent", "success");
    await bluetoothApi().send(POWER_OFF_UUID, [...POWER_OFF_COMMAND], "withResponse", SERVICE_UUID);
  } catch (cause) {
    powerOffPending = false;
    connection = previousConnection;
    error = message(cause);
    addActivity(error, "error");
    render();
  }
}

/** Clears every live value because a disconnected dashboard must not present a stale node as healthy. */
function onDisconnect(attempt?: number): void {
  if (attempt !== undefined && attempt !== connectionAttempt) return;

  if (selectedAddress !== null) addActivity("Connection to Amaru closed");
  powerOffPending = false;
  connection = "disconnected";
  snapshot = null;
  lastPayloadAt = null;
  lastTipHash = null;
  lastTipUpdateAt = null;
  stream.reset();
  selectedAddress = null;
  render();
}

/**
 * Promotes only complete, validated snapshots. Fragment errors stay local to
 * the current update and cannot corrupt the previously rendered dashboard.
 */
function receiveNotification(notification: number[]): void {
  try {
    const next = stream.push(notification, (payload) => decodeSnapshot(decode(payload)));
    if (next !== null) {
      const now = Date.now();
      const receivedFirstSnapshot = snapshot === null;
      snapshot = next;
      lastPayloadAt = now;
      if (next.tip !== null && next.tip.headerHash !== lastTipHash) {
        lastTipHash = next.tip.headerHash;
        lastTipUpdateAt = now;
      }
      if (connection !== "shutting_down") connection = "connected";
      error = null;
      if (receivedFirstSnapshot) addActivity("Receiving live telemetry", "success");
      if (connection !== "shutting_down" && !powerOffPending) render();
    }
  } catch (cause) {
    error = message(cause);
    addActivity(error, "error");
    if (connection !== "shutting_down" && !powerOffPending) render();
  }
}

function render(): void {
  app.innerHTML = connection === "shutting_down" ? shutdownView() : snapshot === null ? setupView() : dashboardView(snapshot);
  bindActions();
}

function shutdownView(): string {
  return `
    <section class="shell setup">
      <header class="masthead">
        <img class="brand-logo" src="${amaruLogo}" alt="" />
        <div><p class="eyebrow">Cardano. Everywhere.</p><h1>Amaru</h1></div>
      </header>
      <p class="loading"><i></i>Shutting down Amaru...</p>
    </section>`;
}

function setupView(): string {
  const waitingForTelemetry = connection === "connecting" || connection === "awaiting";
  const discoveringInitialNode = initialDiscoveryPending && (bluetooth === null || connection === "scanning");
  const nodes = [...devices.values()]
    .sort((left, right) => right.rssi - left.rssi)
    .map(
      (device) => `
        <button class="device" data-connect="${escape(device.address)}">
          <span class="device__mark"></span>
          <span>
            <strong>${escape(device.name || "Amaru node")}</strong>
            <small>${escape(device.address)}</small>
          </span>
          <span class="chevron">›</span>
        </button>`,
    )
    .join("");
  const busy = connection === "scanning" || waitingForTelemetry;
  const unavailable = !hasTauriRuntime;

  return `
    <section class="shell setup">
      <header class="masthead">
        <img class="brand-logo" src="${amaruLogo}" alt="" />
        <div><p class="eyebrow">Cardano. Everywhere.</p><h1>Amaru</h1></div>
      </header>
      <div class="setup-copy">
        <p>Connect to a nearby Amaru node over <i class="accent-cyan">Bluetooth</i>.</p>
      </div>
      ${waitingForTelemetry ? `<p class="loading"><i></i>${connection === "connecting" ? "Connecting to Amaru..." : "Waiting for telemetry..."}</p>` : discoveringInitialNode ? `<p class="loading"><i></i>Searching nearby Amaru nodes...</p>` : `
        <button class="primary${connection === "scanning" ? " primary--scanning" : ""}" data-scan ${busy || unavailable ? "disabled" : ""}>
          ${unavailable ? "Open in Amaru Mobile" : connection === "scanning" ? "Scanning nearby nodes..." : "Find Amaru node"}
        </button>`}
      <section class="found" aria-live="polite">
        ${waitingForTelemetry ? "" : nodes || ""}
      </section>
      ${activityLog()}
      ${unavailable ? '<p class="notice">Bluetooth is available only from the native Tauri application. Start it with <code>npm run tauri dev</code> for macOS or <code>npm run tauri ios dev</code> for an iPhone.</p>' : ""}
      ${error === null ? "" : `<p class="error">${escape(error)}</p>`}
    </section>`;
}

function dashboardView(current: Snapshot): string {
  const resource = current.resource;
  const peers = current.peers.map(peerRow).join("") || '<tr><td colspan="4" class="muted">No peer telemetry yet.</td></tr>';
  const tip = current.tip;
  const signalLost = lastPayloadAt === null || Date.now() - lastPayloadAt > SIGNAL_TIMEOUT_MS;

  return `
    <section class="shell dashboard">
      <header class="topbar">
        <img class="brand-logo brand-logo--compact" src="${amaruLogo}" alt="" />
        <div class="node-name"><strong>AMARU</strong><span>${escape(current.node.version.replace(/^amaru\s+/i, ""))}</span></div>
        <span class="connection ${signalLost ? "connection--lost" : ""}"><i></i>${signalLost ? "no signal" : lastTipAge()}</span>
        ${current.powerOffEnabled ? powerOffControl(powerOffPending) : ""}
      </header>

      ${nodeCard(current.node, resource)}

      <section class="card-grid">
        ${tipCard(tip)}
        ${detailsCard("Chain quality", [
          ["Density", tip === null ? "-" : `${(tip.density * 100).toFixed(2)}%`],
          ["Rollback depth", current.chainQuality.averageRollbackLength === null ? "-" : current.chainQuality.averageRollbackLength.toFixed(1)],
          ["Rollback frequency", current.chainQuality.rollbackFrequencyPerSecond === null ? "-" : rate(current.chainQuality.rollbackFrequencyPerSecond, "/s")],
        ])}
      </section>

      <section class="card-grid">
        ${detailsCard("Throughput", [
          ["Blocks", current.throughput.blocksPerSecond > EPSILON ? rate(current.throughput.blocksPerSecond, "blk/s") : count(current.throughput.blocks)],
          ["Transactions", current.throughput.transactionsPerSecond > EPSILON ? rate(current.throughput.transactionsPerSecond, "tx/s") : count(current.throughput.transactions)],
        ])}
        ${detailsCard("Mempool", [
          ["Transactions", count(current.mempool.transactions)],
          ["Occupancy", bytes(current.mempool.sizeBytes)],
        ])}
      </section>

      <section class="card peers">
        <div class="card__title">Peers</div>
        <div class="table-wrap">
          <table>
            <thead><tr><th></th><th>Name</th><th>RTT</th></tr></thead>
            <tbody>${peers}</tbody>
          </table>
        </div>
      </section>
      ${error === null ? "" : `<p class="error">${escape(error)}</p>`}
    </section>`;
}

function nodeCard(node: Snapshot["node"], resource: Snapshot["resource"]): string {
  return `<section class="card node">
    <div class="card__title">Node</div>
    ${details([
      ["PID", String(node.pid)],
      ["Uptime", uptime(node.uptimeSeconds)],
      ["Network", node.network],
    ])}
    <div class="node__metrics">
      ${metric("Memory", resource === null ? "-" : bytes(resource.processMemoryBytes), resource === null ? null : percent(resource.processMemoryBytes, resource.hostMemoryTotalBytes))}
      ${metric("CPU", resource === null ? "-" : `${resource.cpuPercent.toFixed(1)}%`, resource === null ? null : resource.cpuPercent)}
      ${metric(
        "Disk read",
        resource === null ? "-" : dataRate(resource.processDiskReadBytes),
        resource === null ? null : percent(resource.processDiskReadBytes, resource.hostDiskReadBytes),
      )}
      ${metric(
        "Disk write",
        resource === null ? "-" : dataRate(resource.processDiskWriteBytes),
        resource === null ? null : percent(resource.processDiskWriteBytes, resource.hostDiskWriteBytes),
      )}
      ${temperatureMetric(resource)}
    </div>
  </section>`;
}

function powerOffControl(switchingOff: boolean): string {
  return `<button class="power-button${switchingOff ? " power-button--switching-off" : ""}" data-power-off type="button" aria-label="Power off node" title="Power off node"${switchingOff ? " disabled" : ""}>
    <svg class="power-button__icon power-button__icon--off" viewBox="0 0 150 150" aria-hidden="true">
      <line class="power-button__line" x1="75" y1="34" x2="75" y2="58" />
      <circle class="power-button__circle" cx="75" cy="80" r="35" />
    </svg>
    <svg class="power-button__icon power-button__icon--on" viewBox="0 0 150 150" aria-hidden="true">
      <line class="power-button__line" x1="75" y1="34" x2="75" y2="58" />
      <circle class="power-button__circle" cx="75" cy="80" r="35" />
    </svg>
  </button>`;
}

function metric(label: string, value: string, valueAsPercent: number | null, markerPercent: number | null = null): string {
  const boundedPercent = valueAsPercent === null ? null : Math.min(100, Math.max(0, valueAsPercent));
  const boundedMarker = markerPercent === null ? null : Math.min(100, Math.max(0, markerPercent));
  const detail = boundedPercent === null ? "-" : `${boundedPercent.toFixed(1)}%`;
  const progress = boundedPercent === null ? "" : `<i class="metric__bar"><i style="width:${boundedPercent}%"></i>${boundedMarker === null ? "" : `<b style="left:${boundedMarker}%"></b>`}</i>`;
  return `<div class="metric" title="${escape(`${label}: ${value}`)}">
    <div class="metric__heading"><span>${escape(label)}</span><strong>${escape(value)}</strong></div>
    <div class="metric__progress">${progress}<small>${detail}</small></div>
  </div>`;
}

function temperatureMetric(resource: Snapshot["resource"]): string {
  const average = resource?.averageTemperatureCelsius ?? null;
  const maximum = resource?.maximumTemperatureCelsius ?? null;
  const value = average === null ? "-" : `avg=${celsius(average)}`;

  return metric("Temperature", value, temperaturePercent(average), temperaturePercent(maximum));
}

function celsius(value: number | null): string {
  return value === null ? "-" : `${value.toFixed(0)}°`;
}

function temperaturePercent(value: number | null): number | null {
  return value === null ? null : (value / 110) * 100;
}

function tipCard(tip: Snapshot["tip"]): string {
  return detailsCard(
    "Local tip",
    tip === null
      ? [["Status", "Waiting for a tip.update trace."]]
      : [
          ["Epoch", String(tip.epoch)],
          ["Slot", count(tip.slot)],
          ["Height", count(tip.blockHeight)],
          ["Hash", tip.headerHash.slice(0, 16)],
        ],
  );
}

function detailsCard(title: string, entries: [string, string][]): string {
  return `<section class="card"><div class="card__title">${escape(title)}</div>${details(entries)}</section>`;
}

function details(entries: [string, string][]): string {
  return `<dl class="details">${entries
    .map(([name, value]) => `<div><dt>${escape(name)}</dt><dd>${escape(value)}</dd></div>`)
    .join("")}</dl>`;
}

function peerRow(peer: Snapshot["peers"][number]): string {
  const direction = `${peer.outbound ? "↓" : ""}${peer.inbound ? "↑" : ""}` || "-";
  return `<tr>
    <td><i class="peer-state ${peer.connected ? "online" : "offline"}"></i> ${direction}</td>
    <td title="${escape(peer.address)}">${escape(truncate(peer.address, 24))}</td>
    <td>${duration(peer.rttMicros)}</td>
  </tr>`;
}

function truncate(value: string, limit: number): string {
  return value.length <= limit ? value : `${value.slice(0, limit - 1)}…`;
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, milliseconds));
}

function bindActions(): void {
  app.querySelector<HTMLButtonElement>("[data-scan]")?.addEventListener("click", () => void discover(false));
  app.querySelector<HTMLButtonElement>("[data-power-off]")?.addEventListener("click", () => void powerOff());
  for (const element of app.querySelectorAll<HTMLButtonElement>("[data-connect]")) {
    element.addEventListener("click", () => void attach(element.dataset.connect ?? ""));
  }
}

/**
 * Retains only recent connection milestones: activity explains a stalled setup
 * without turning this status display into another unbounded log collector.
 */
function addActivity(message: string, level: ActivityLevel = "info"): void {
  const latest = activities[0];
  if (latest?.message === message && latest.level === level) return;

  activities.unshift({ level, message, timestamp: Date.now() });
  activities.length = Math.min(activities.length, ACTIVITY_CAPACITY);
}

function activityLog(): string {
  if (activities.length === 0) return "";

  return `<section class="activity-log" aria-live="polite">
    <div class="activity-log__title">Activity</div>
    ${activities
      .slice()
      .reverse()
      .map((activity) => `<p class="activity-log__line activity-log__line--${activity.level}"><time>${activityTime(activity.timestamp)}</time><i></i>${escape(activity.message)}</p>`)
      .join("")}
  </section>`;
}

function activityTime(timestamp: number): string {
  return new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" }).format(timestamp);
}

/**
 * Prefers the service UUID but accepts the advertised name as a fallback for
 * platforms that omit service UUIDs from scan results.
 */
function isAmaru(device: BleDevice): boolean {
  return device.services.some((service) => service.toLowerCase() === SERVICE_UUID) || device.name.toLowerCase().includes("amaru");
}

function bluetoothApi(): Bluetooth {
  if (bluetooth !== null) return bluetooth;
  if (hasTauriRuntime) throw new Error("Bluetooth is still initialising");
  throw new Error("Bluetooth is available only from the native Amaru Mobile application");
}

function lastTipAge(): string {
  if (lastTipUpdateAt === null) return "waiting for tip";
  return `${Math.max(0, Math.floor((Date.now() - lastTipUpdateAt) / 1_000))}s ago`;
}

function message(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function escape(value: string): string {
  return value.replace(/[&<>'"]/g, (character) => {
    const entities: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" };
    return entities[character];
  });
}

function element(selector: string): HTMLElement {
  const selected = document.querySelector<HTMLElement>(selector);
  if (selected === null) throw new Error(`Missing required element: ${selector}`);
  return selected;
}
