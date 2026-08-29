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

//! Local process and host sampling for the resource cards absent from JSON traces.

use anyhow::{Context, anyhow};
use sysinfo::{
    Components, CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate,
    RefreshKind, System,
};

use crate::projection::ResourceSample;

/// Resolves an explicitly configured PID, or the first process named `amaru`.
///
/// The explicit value takes precedence so a host running multiple nodes never
/// accidentally advertises health from the wrong process.
pub fn resolve_pid(pid: Option<u32>) -> anyhow::Result<Pid> {
    if let Some(pid) = pid {
        return Ok(Pid::from_u32(pid));
    }

    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system
        .processes()
        .iter()
        .find_map(|(pid, process)| (process.name().to_string_lossy() == "amaru").then_some(*pid))
        .ok_or_else(|| anyhow!("no process named 'amaru' found; pass --pid explicitly"))
}

/// Returns the installed Amaru version once at bridge startup.
///
/// The version changes only on deployment, so running the command once avoids
/// putting process creation in the one-second sampling path.
pub async fn amaru_version() -> anyhow::Result<String> {
    let output =
        tokio::process::Command::new("amaru").arg("--version").output().await.context("run `amaru --version`")?;
    anyhow::ensure!(output.status.success(), "`amaru --version` exited with {}", output.status);

    let version = String::from_utf8(output.stdout).context("decode `amaru --version` output")?;
    let version = version.trim();
    anyhow::ensure!(!version.is_empty(), "`amaru --version` produced no output");
    Ok(version.to_owned())
}

/// Reuses `sysinfo` state to produce one low-overhead sample per second.
pub struct Sampler {
    pid: Pid,
    system: System,
    disks: Disks,
    components: Components,
    cpu_count: f64,
}

impl Sampler {
    /// Builds a sampler around the fields required by the mobile view only.
    ///
    /// Restricting refresh kinds keeps bridge overhead predictable on small
    /// hosts and avoids treating sysinfo as a general process monitor.
    pub fn new(pid: u32) -> Self {
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything().without_frequency())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        let cpu_count = system.cpus().len().max(1) as f64;
        Self {
            pid: Pid::from_u32(pid),
            system,
            disks: Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_io_usage()),
            components: Components::new_with_refreshed_list(),
            cpu_count,
        }
    }

    /// Collects the next host-health sample without retaining previous samples.
    ///
    /// Disk counters come from the same sampling pass as the process counters,
    /// which lets the dashboard compare them without mismatched intervals.
    pub fn sample(&mut self) -> Option<ResourceSample> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_disk_usage().with_memory(),
        );
        self.system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        self.disks.refresh_specifics(false, DiskRefreshKind::nothing().with_io_usage());
        self.components.refresh(false);

        let process = self.system.process(self.pid)?;
        let (average_temperature_celsius, maximum_temperature_celsius) = temperature_range(&self.components);
        let disk = process.disk_usage();
        let (host_disk_read_bytes, host_disk_write_bytes) = self.disks.iter().fold((0_u64, 0_u64), |totals, disk| {
            let usage = disk.usage();
            (totals.0.saturating_add(usage.read_bytes), totals.1.saturating_add(usage.written_bytes))
        });

        Some(ResourceSample {
            runtime_seconds: process.run_time(),
            cpu_percent: process.cpu_usage() as f64 / self.cpu_count,
            // Linux exposes RSS as process memory. There is no equivalent compact footprint value.
            process_memory_bytes: process.memory(),
            rss_bytes: process.memory(),
            virtual_bytes: process.virtual_memory(),
            host_memory_used_bytes: self.system.used_memory(),
            host_memory_total_bytes: self.system.total_memory(),
            process_disk_read_bytes: disk.read_bytes,
            process_disk_write_bytes: disk.written_bytes,
            host_disk_read_bytes,
            host_disk_write_bytes,
            average_temperature_celsius,
            maximum_temperature_celsius,
        })
    }
}

fn temperature_range(components: &Components) -> (Option<f32>, Option<f32>) {
    let mut temperatures = components
        .iter()
        .filter_map(|component| component.temperature())
        .filter(|temperature| temperature.is_finite() && *temperature > 0.0);
    let Some(first) = temperatures.next() else {
        return (None, None);
    };

    let (total, count, maximum) = temperatures.fold((first, 1_u32, first), |(total, count, maximum), temperature| {
        (total + temperature, count + 1, maximum.max(temperature))
    });

    (Some(total / count as f32), Some(maximum))
}
