use netem_trace::Bandwidth;
use rattan_core::{
    cells::Packet,
    config::{BwCellBuildConfig, BwReplayCellBuildConfig, CellBuildConfig},
    error::{Error::Custom, Result},
};
use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    iter,
    time::Duration,
};
use tracing::warn;

// Define output format of an loss item in the .csv file here
fn express_loss(loss: Vec<f64>) -> String {
    format!("{:?}", loss)
}

// Define output format of an bandwidth item in the .csv file here
fn express_bandwidth(bw: Bandwidth) -> String {
    format!("{:?}", bw)
}
// Define output format of an delay item in the .csv file here
fn express_delay(delay: Duration) -> String {
    format!("{:?}", delay)
}

// Define output format of the trace logical timestamp in the .csv file here
fn express_time(time: Duration) -> String {
    format!("{:?}", time.as_nanos())
}

pub fn write_combined_trace<P: Packet>(
    output: impl Write,
    cells: HashMap<String, CellBuildConfig<P>>,
    length: Duration,
) -> Result<()> {
    let mut cells_to_report = vec![];

    let mut change_points: BTreeMap<Duration, Vec<(usize, String)>> = BTreeMap::new();

    let mut add_change_point = |timestamp: Duration, cell_id: usize, value: String| {
        change_points
            .entry(timestamp)
            .or_default()
            .push((cell_id, value));
    };

    for (name, cell) in cells {
        cells_to_report.push(name);
        let cell_id = cells_to_report.len() - 1; // index in `cells_to_report`.

        match cell {
            CellBuildConfig::Loss(config) => {
                let loss = config.pattern;
                add_change_point(Duration::ZERO, cell_id, express_loss(loss));
            }

            CellBuildConfig::Delay(config) => {
                let delay = config.delay;
                add_change_point(Duration::ZERO, cell_id, express_delay(delay));
            }
            CellBuildConfig::Bw(config) => {
                let bandwidth = match config {
                    BwCellBuildConfig::CoDel(config) => config.bandwidth,
                    BwCellBuildConfig::Infinite(config) => config.bandwidth,
                    BwCellBuildConfig::DropHead(config) => config.bandwidth,
                    BwCellBuildConfig::DropTail(config) => config.bandwidth,
                }
                .unwrap();
                add_change_point(Duration::ZERO, cell_id, express_bandwidth(bandwidth));
            }

            CellBuildConfig::LossReplay(config) => {
                let mut trace = config.get_trace()?;
                let mut now = Duration::ZERO;
                while let Some((loss, time)) = trace.next_loss() {
                    if now >= length {
                        break;
                    }
                    add_change_point(now, cell_id, express_loss(loss));
                    now += time;
                }
            }

            CellBuildConfig::DelayReplay(config) => {
                let mut trace = config.get_trace()?;
                let mut now = Duration::ZERO;
                while let Some((delay, time)) = trace.next_delay() {
                    if now >= length {
                        break;
                    }
                    add_change_point(now, cell_id, express_delay(delay));
                    now += time;
                }
            }

            CellBuildConfig::BwReplay(config) => {
                let mut trace = match config {
                    BwReplayCellBuildConfig::CoDel(config) => config.get_trace(),
                    BwReplayCellBuildConfig::DropHead(config) => config.get_trace(),
                    BwReplayCellBuildConfig::DropTail(config) => config.get_trace(),
                    BwReplayCellBuildConfig::Infinite(config) => config.get_trace(),
                }
                .unwrap();
                let mut now = Duration::ZERO;
                while let Some((bw, time)) = trace.next_bw() {
                    if now >= length {
                        break;
                    }
                    add_change_point(now, cell_id, express_bandwidth(bw));
                    now += time;
                }
            }
            _ => {
                warn!(
                    "Cell {:?} not supported for generate combined trace",
                    cells_to_report.pop()
                );
            }
        }
    }

    let mut csv_writer = csv::Writer::from_writer(output);

    csv_writer
        .write_record(iter::once("time_since_ns").chain(cells_to_report.iter().map(String::as_str)))
        .map_err(|e| Custom(format!("{:?}", e)))?;

    let mut current_state = vec![String::new(); cells_to_report.len()];

    for (time, changes) in change_points {
        for (cell_id, change_to) in changes {
            current_state[cell_id] = change_to;
        }
        csv_writer
            .write_record(iter::once(&express_time(time)).chain(current_state.iter()))
            .map_err(|e| Custom(format!("{:?}", e)))?;
    }

    Ok(())
}
