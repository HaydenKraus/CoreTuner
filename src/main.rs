#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]
//! CoreTuner — управление affinity и приоритетами процессов Windows.

mod app;
mod autostart;
mod config;
mod cpu;
mod i18n;
mod logging;
mod model;
mod process;
mod profiles;
mod single;
mod tray;
mod ui;

use std::sync::Arc;

fn main() {
    logging::init();
    if let Err(e) = run() {
        tracing::error!("fatal: {e:#}");
        let _ = rfd::MessageDialog::new()
            .set_title("CoreTuner")
            .set_description(format!("{e:#}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}

fn run() -> anyhow::Result<()> {
    let minimized_flag = std::env::args().any(|a| a == "--minimized");

    let restarted = std::env::args().any(|a| a == "--restarted");
    let acquired = if restarted {
        single::acquire_wait(std::time::Duration::from_secs(10))
    } else {
        single::acquire()
    };
    let _guard = match acquired {
        single::Instance::First(g) => g,
        single::Instance::Already => {
            single::signal_existing();
            return Ok(());
        }
    };

    let cfg = profiles::storage::load();
    let topo = Arc::new(cpu::topology::Topology::detect()?);
    let start_hidden = minimized_flag && cfg.settings.start_minimized;

    let (show_tx, show_rx) = crossbeam_channel::unbounded::<()>();
    single::spawn_show_listener(move || {
        let _ = show_tx.send(());
    });

    let viewport = egui::ViewportBuilder::default()
        .with_title("CoreTuner")
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([860.0, 560.0])
        .with_visible(!start_hidden)
        .with_drag_and_drop(true);
    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "CoreTuner",
        options,
        Box::new(move |cc| Ok(Box::new(app::CoreTunerApp::new(cc, cfg, topo, start_hidden, show_rx)))),
    )
    .map_err(|e| anyhow::anyhow!("не удалось запустить окно: {e}"))
}


