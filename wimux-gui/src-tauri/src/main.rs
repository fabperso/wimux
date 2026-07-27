// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fabrice Andy
// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    wimux_gui_lib::run()
}
