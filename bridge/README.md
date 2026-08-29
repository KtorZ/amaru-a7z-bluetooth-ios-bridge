# Amaru mobile telemetry bridge

`amaru-mobile-telemetry` follows Amaru's JSON traces in the system journal,
reduces them to the state required by the **Amaru** dashboard, and publishes a
versioned CBOR snapshot through a Bluetooth Low Energy GATT notification
characteristic. It never queries Amaru's database or control interfaces: the
mobile view is intentionally constrained to what a public telemetry consumer
can observe.

It produces one fresh snapshot per second, caps the encoded snapshot at 7 KiB,
and fragments it into 160-byte payloads for conservative BLE MTUs. Fragments
are spaced by 20 ms so receivers can drain them; the complete wire rate,
including fragment headers, stays below 10 KiB/s.

The stream accepts one notification subscriber at a time, so the service-wide
BLE output remains within that budget as well.

The service consumes only Amaru's public telemetry schema names and generated
field constants. Unknown, malformed, or unrelated JSON lines are ignored so
new high-volume traces cannot make the mobile service retain or process an
unbounded amount of data.

## Trace source

Run Amaru with JSON traces. The bridge reads `amaru.service` through
`journalctl`, so no trace file or log-rotation configuration is required. It
samples process and host resource data itself because those values are not part
of the JSON trace stream.

The bridge rebuilds its in-memory dashboard projection from the most recent
4,096 journal entries on every start, preserving each journal timestamp. It
then checkpoints the final cursor under its systemd state directory and follows
new entries from there. This restores the current tip, peers, and mempool, and
reconstructs recent throughput and rollback statistics without treating the
replay itself as live work. The replay bound is also the bridge's recovery
budget: it avoids replaying an unbounded journal after a long outage.

```ini
# /etc/systemd/system/amaru.service.d/mobile-telemetry.conf
[Service]
Environment=AMARU_WITH_JSON_TRACES=true
Environment=AMARU_TRACE=warn,amaru=debug,amaru_pure_stage=warn
StandardOutput=journal
```

The `debug` trace filter is necessary for `tip.update`, which provides the
debounced block and transaction deltas used for throughput. This is an opt-in
operator setup: JSON traces are materially more verbose than normal node
logging.

The bridge unit grants its `amaru` process journal access through the
`systemd-journal` group. Override `AMARU_MOBILE_JOURNAL_UNIT` when the node
runs under another unit name. `AMARU_MOBILE_JOURNAL_CURSOR_FILE` defaults to
the bridge's systemd-managed state directory and can be overridden for a
manual invocation.

## Configuration

Every bridge option has a command-line flag and a matching environment
variable. Environment variables are intended for the systemd unit; flags are
useful for a foreground troubleshooting session.

| Option | Environment variable | Why it exists |
| --- | --- | --- |
| `--journal-unit` | `AMARU_MOBILE_JOURNAL_UNIT` | Follows the right node when Amaru is not installed as `amaru.service`. |
| `--journal-cursor-file` | `AMARU_MOBILE_JOURNAL_CURSOR_FILE` | Preserves the incremental journal position across bridge restarts. |
| `--network` | `AMARU_NETWORK` | Labels the dashboard before a tip event arrives. |
| `--pid` | `AMARU_PID` | Avoids ambiguous process-name discovery on hosts running more than one Amaru. |
| `--adapter` | `AMARU_BLUETOOTH_ADAPTER` | Selects a specific Bluetooth controller on multi-adapter hosts. |
| `--bluetooth-name` | `AMARU_MOBILE_BLUETOOTH_NAME` | Makes the advertised node recognisable in nearby-device scans. |
| `--enable-power-off` | `AMARU_MOBILE_ENABLE_POWER_OFF` | Exposes the deliberately narrow but unauthenticated shutdown capability. |

## Running

```bash
cargo build --manifest-path bridge/Cargo.toml --release
sudo install -m 0755 bridge/target/release/amaru-mobile-telemetry /usr/local/bin/
sudo install -m 0644 bridge/systemd/amaru-mobile-telemetry.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now amaru-mobile-telemetry.service
```

Set `AMARU_PID` when automatic discovery cannot find the node process.

### Optional power-off action

The mobile application can request a host power-off through a distinct GATT
characteristic. The command-line bridge defaults to disabling it; the supplied
systemd unit currently opts in through `AMARU_MOBILE_ENABLE_POWER_OFF=true`.
Change that value to `false` before installing the unit if the capability is
not appropriate for the host.

When enabled, the mobile power button immediately sends one fixed command. It
cannot pass arguments, run arbitrary programs, or operate the node. The action
is deliberately unauthenticated, so any nearby Bluetooth client that knows the
public command can power off the host. Install it only where that availability
risk is acceptable:

```bash
sudo install -m 0644 bridge/systemd/amaru-mobile-poweroff.service /etc/systemd/system/
sudo install -m 0644 bridge/systemd/amaru-mobile-poweroff.socket /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now amaru-mobile-poweroff.socket
sudo systemctl edit amaru-mobile-telemetry.service
```

If the shipped unit has been changed to disable power-off, add the following
drop-in and restart the bridge to opt in:

```ini
[Service]
Environment=AMARU_MOBILE_ENABLE_POWER_OFF=true
```

```bash
sudo systemctl restart amaru-mobile-telemetry.service
```

The bridge runs as `amaru`. The root-owned socket unit exposes a datagram
socket writable only by that user; receiving a datagram activates the root-owned
one-shot power-off unit. This grants neither a shell nor general systemd
control, and does not depend on `sudo` acquiring privilege from the bridge
service.

The one-shot power-off unit is triggered on demand and must not be enabled. Its
socket unit must remain enabled while the Bluetooth action is enabled. Existing
installations can remove the now-unused `/etc/sudoers.d/amaru-mobile-poweroff`
file after installing the socket unit.


### From macOS

Cross-checking the bridge from macOS is useful before deploying to an ARM Linux
host. The Linux build needs D-Bus development files, so use the same container
image as the deployment workflow:

```console
docker run --rm \
  -v "$PWD":/workspace \
  -w /workspace \
  amaru-dev-linux:latest \
  bash -c 'apt-get update && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config && cargo check --manifest-path bridge/Cargo.toml'
```

## Bluetooth contract

| Item | Value |
| --- | --- |
| Service UUID | `8b4cb36a-7a5d-4f9f-8f31-6a5f4fc8c711` |
| Stream UUID | `8b4cb36a-7a5d-4f9f-8f31-6a5f4fc8c712` |
| Power-off UUID | `8b4cb36a-7a5d-4f9f-8f31-6a5f4fc8c713` when `AMARU_MOBILE_ENABLE_POWER_OFF=true` |
| Stream characteristic | Notify |
| Power-off characteristic | Write, exact UTF-8 `amaru/power-off/v1` |
| Snapshot | CBOR array, version `5` |
| Fragment | `0xa7`, version, big-endian sequence, index, count, payload |

The application and bridge currently support only snapshot version `5`. Deploy
them as a matched pair when the protocol changes: the mobile application
rejects snapshots from another version instead of attempting partial decoding.

The Rust golden test writes raw CBOR snapshots to `target/test-vectors/`. The
mobile application's `npm test` runs that producer before decoding the same
artifacts in TypeScript. This makes the Rust encoder the source of test vectors
and catches schema drift on either side of the language boundary.

BlueZ's local GATT notification API does not expose an encryption requirement
for notification characteristics, so the telemetry stream must be treated as
local, trusted-network data. It should not be enabled where disclosure of peer
addresses or node health is unacceptable. The optional power-off action is also
unauthenticated and exposes a deliberate nearby-device denial-of-service risk.
