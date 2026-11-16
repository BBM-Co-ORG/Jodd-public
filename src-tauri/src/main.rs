// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if jodd_lib::backend::ssh::askpass::maybe_run_as_askpass() {
        return;
    }
    jodd_lib::run();
}
