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

//! Narrow local handoff for the Bluetooth power-off command.

use anyhow::Context;
use tokio::net::UnixDatagram;

const SOCKET_PATH: &str = "/run/amaru-mobile-poweroff.sock";

/// Exact, versioned control value accepted by the power-off characteristic.
///
/// This is not a secret or an authentication mechanism. It merely prevents a
/// malformed or unrelated GATT write from becoming a shutdown request.
pub const COMMAND: &[u8] = b"amaru/power-off/v1";

/// True only for the one control value supported by the bridge.
pub fn is_command(value: &[u8]) -> bool {
    value == COMMAND
}

/// Asks the dedicated root-owned systemd unit to power off the host.
///
/// The socket is writable only by the bridge user and activates the dedicated
/// one-shot unit. This avoids relying on a setuid `sudo` transition from the
/// restricted bridge service.
pub async fn schedule() -> anyhow::Result<()> {
    let socket = UnixDatagram::unbound().context("create power-off socket")?;
    socket.send_to(COMMAND, SOCKET_PATH).await.context("request power-off service")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_fixed_command() {
        assert!(is_command(COMMAND));
        assert!(!is_command(b"amaru/power-off/v2"));
        assert!(!is_command(b"poweroff"));
    }
}
