/**
 * Keeps byte values readable without making a compact metric card depend on a
 * particular host memory size.
 */
export function bytes(value: number): string {
  if (value < 1_024) return `${value.toFixed(0)} B`;
  if (value < 1_024 ** 2) return `${(value / 1_024).toFixed(1)} KiB`;
  if (value < 1_024 ** 3) return `${(value / 1_024 ** 2).toFixed(1)} MiB`;
  return `${(value / 1_024 ** 3).toFixed(2)} GiB`;
}

/**
 * Limits precision for live rates so small fluctuations do not make the
 * dashboard appear noisier than the one-second telemetry cadence.
 */
export function rate(value: number, unit: string): string {
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${unit}`;
}

/** Preserves the byte formatter's units while making the sampling interval explicit. */
export function dataRate(value: number): string {
  return `${bytes(value)}/s`;
}

/** Uses grouping because node counters quickly become hard to scan as raw digits. */
export function count(value: number): string {
  return new Intl.NumberFormat("en-US").format(value);
}

/**
 * Chooses a unit that makes latency differences legible without requiring the
 * operator to mentally convert microseconds.
 */
export function duration(micros: number | null): string {
  if (micros === null) return "-";
  if (micros < 1_000) return `${micros.toFixed(0)} us`;
  if (micros < 1_000_000) return `${(micros / 1_000).toFixed(1)} ms`;
  return `${(micros / 1_000_000).toFixed(2)} s`;
}

/**
 * Avoids a date-time because uptime is used as a liveness cue, not an audit
 * timestamp.
 */
export function uptime(seconds: number | null): string {
  if (seconds === null) return "-";
  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  return hours === 0 ? `${minutes} min` : `${hours} h ${minutes} min`;
}

/**
 * Protects presentation code from division by zero while a host sampler is
 * still warming up or cannot report a total.
 */
export function percent(value: number, total: number): number {
  return total === 0 ? 0 : (value / total) * 100;
}
