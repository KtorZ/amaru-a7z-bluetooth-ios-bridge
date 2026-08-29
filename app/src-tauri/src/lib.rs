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

//! Minimal native shell for the Bluetooth dashboard.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Registers only the BLE plugin so the web view remains the sole owner of dashboard state.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_blec::init())
        .run(tauri::generate_context!())
        .expect("failed to run Amaru Mobile")
}
