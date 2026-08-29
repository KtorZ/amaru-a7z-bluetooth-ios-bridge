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

//! BlueZ GATT service that publishes the most recently projected snapshot.

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bluer::{
    adv::Advertisement,
    gatt::{
        WriteOp,
        local::{
            Application, Characteristic, CharacteristicNotify, CharacteristicNotifyMethod, CharacteristicWrite,
            CharacteristicWriteMethod, ReqError, Service,
        },
    },
};
use tokio::sync::watch;
use uuid::Uuid;

use crate::power_off;

/// Stable Amaru mobile telemetry service UUID.
///
/// The UUID identifies the protocol independently of the advertised name,
/// which operators are free to customise.
pub const SERVICE_UUID: Uuid = Uuid::from_u128(0x8b4cb36a_7a5d_4f9f_8f31_6a5f4fc8c711);

/// Notification-only stream characteristic UUID, keeping telemetry read-only.
pub const STREAM_UUID: Uuid = Uuid::from_u128(0x8b4cb36a_7a5d_4f9f_8f31_6a5f4fc8c712);

/// Writable characteristic for the one fixed power-off command when explicitly enabled.
pub const POWER_OFF_UUID: Uuid = Uuid::from_u128(0x8b4cb36a_7a5d_4f9f_8f31_6a5f4fc8c713);

/// Leaves enough time for conservative BLE receivers to drain each fragment.
const NOTIFICATION_INTERVAL: Duration = Duration::from_millis(20);

/// Advertises and serves the latest snapshot until the process is terminated.
///
/// Telemetry and the optional control characteristic share one service so a
/// mobile client can discover the complete local contract with one UUID.
pub async fn serve(
    snapshots: watch::Receiver<Vec<Vec<u8>>>,
    adapter_name: Option<&str>,
    local_name: &str,
    enable_power_off: bool,
) -> anyhow::Result<()> {
    let session = bluer::Session::new().await?;
    let adapter = match adapter_name {
        Some(name) => session.adapter(name)?,
        None => session.default_adapter().await?,
    };
    adapter.set_powered(true).await?;

    let advertisement = adapter
        .advertise(Advertisement {
            service_uuids: BTreeSet::from([SERVICE_UUID]),
            discoverable: Some(true),
            local_name: Some(local_name.to_owned()),
            ..Default::default()
        })
        .await?;
    let application = adapter.serve_gatt_application(application(snapshots, enable_power_off)).await?;

    eprintln!(
        "Amaru mobile telemetry available on {} ({}) power_off_enabled={enable_power_off}",
        adapter.name(),
        adapter.address().await?
    );
    tokio::signal::ctrl_c().await?;
    drop(application);
    drop(advertisement);
    Ok(())
}

fn application(snapshots: watch::Receiver<Vec<Vec<u8>>>, enable_power_off: bool) -> Application {
    let subscriber_active = Arc::new(AtomicBool::new(false));
    let mut characteristics = vec![stream_characteristic(snapshots, subscriber_active)];
    if enable_power_off {
        characteristics.push(power_off_characteristic(Arc::new(AtomicBool::new(false))));
    }

    Application {
        services: vec![Service { uuid: SERVICE_UUID, primary: true, characteristics, ..Default::default() }],
        ..Default::default()
    }
}

fn stream_characteristic(
    snapshots: watch::Receiver<Vec<Vec<u8>>>,
    subscriber_active: Arc<AtomicBool>,
) -> Characteristic {
    Characteristic {
        uuid: STREAM_UUID,
        notify: Some(CharacteristicNotify {
            notify: true,
            method: CharacteristicNotifyMethod::Fun(Box::new(move |mut notifier| {
                let mut snapshots = snapshots.clone();
                let subscriber_active = subscriber_active.clone();
                Box::pin(async move {
                    // The wire budget covers one adjacent operator. Reject a second
                    // subscription instead of multiplying the advertised rate.
                    if subscriber_active.swap(true, Ordering::Relaxed) {
                        return;
                    }
                    tokio::spawn(async move {
                        loop {
                            let frames = snapshots.borrow_and_update().clone();
                            for (index, frame) in frames.into_iter().enumerate() {
                                if index != 0 {
                                    tokio::time::sleep(NOTIFICATION_INTERVAL).await;
                                }
                                if notifier.notify(frame).await.is_err() {
                                    subscriber_active.store(false, Ordering::Relaxed);
                                    return;
                                }
                            }

                            if snapshots.changed().await.is_err() {
                                subscriber_active.store(false, Ordering::Relaxed);
                                return;
                            }
                        }
                    });
                })
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn power_off_characteristic(power_off_requested: Arc<AtomicBool>) -> Characteristic {
    Characteristic {
        uuid: POWER_OFF_UUID,
        write: Some(CharacteristicWrite {
            write: true,
            write_without_response: true,
            method: CharacteristicWriteMethod::Fun(Box::new(move |value, request| {
                let power_off_requested = power_off_requested.clone();
                Box::pin(async move {
                    if request.offset != 0
                        || !matches!(request.op_type, WriteOp::Request | WriteOp::Command)
                        || !power_off::is_command(&value)
                    {
                        return Err(ReqError::NotPermitted);
                    }

                    if power_off_requested.swap(true, Ordering::AcqRel) {
                        return Err(ReqError::InProgress);
                    }

                    eprintln!("amaru-mobile-telemetry: power-off requested");
                    if let Err(error) = power_off::schedule().await {
                        power_off_requested.store(false, Ordering::Release);
                        eprintln!("amaru-mobile-telemetry: power-off request failed: {error:#}");
                        return Err(ReqError::Failed);
                    }

                    Ok(())
                })
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}
