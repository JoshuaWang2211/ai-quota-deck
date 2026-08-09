// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if ai_quota_deck_lib::run_native_host_if_requested() {
        return;
    }
    ai_quota_deck_lib::run()
}
