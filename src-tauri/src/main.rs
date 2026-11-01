// No console window behind the tray app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    talyvor_desktop_lib::run()
}
