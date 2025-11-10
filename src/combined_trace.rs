use netem_trace::{
    series::{expand_bw_trace, expand_delay_trace, expand_loss_trace},
    Bandwidth,
};
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
    loss.iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(";")
}

// Define output format of an bandwidth item in the .csv file here
fn express_bandwidth(bw: Bandwidth) -> String {
    format!("{:?}", bw.as_bps())
}
// Define output format of an delay item in the .csv file here
fn express_delay(delay: Duration) -> String {
    format!("{:?}", delay.as_secs_f64())
}

// Define output format of the trace logical timestamp in the .csv file here
fn express_time(time: Duration) -> String {
    format!("{:?}", time.as_secs_f64())
}

const LOSS_TRACE_SUFFIX: &str = "_loss_pattern";
const BW_TRACE_SUFFIX: &str = "_bandwidth_bps";
const DELAY_TRACE_SUFFIX: &str = "_delay_secs";

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

    for (mut name, cell) in cells {
        let cell_id = cells_to_report.len(); // index in `cells_to_report`.

        let column_suffix = match cell {
            CellBuildConfig::Loss(config) => {
                let loss = config.pattern;
                add_change_point(Duration::ZERO, cell_id, express_loss(loss));
                LOSS_TRACE_SUFFIX
            }

            CellBuildConfig::Delay(config) => {
                let delay = config.delay;
                add_change_point(Duration::ZERO, cell_id, express_delay(delay));
                DELAY_TRACE_SUFFIX
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
                BW_TRACE_SUFFIX
            }

            CellBuildConfig::LossReplay(config) => {
                let mut trace = config.get_trace()?;

                for trace_point in expand_loss_trace(trace.as_mut(), Duration::ZERO, length) {
                    add_change_point(
                        trace_point.start_time,
                        cell_id,
                        express_loss(trace_point.value),
                    )
                }
                LOSS_TRACE_SUFFIX
            }

            CellBuildConfig::DelayReplay(config) => {
                let mut trace = config.get_trace()?;
                for trace_point in expand_delay_trace(trace.as_mut(), Duration::ZERO, length) {
                    add_change_point(
                        trace_point.start_time,
                        cell_id,
                        express_delay(trace_point.value),
                    )
                }
                DELAY_TRACE_SUFFIX
            }

            CellBuildConfig::BwReplay(config) => {
                let mut trace = match config {
                    BwReplayCellBuildConfig::CoDel(config) => config.get_trace(),
                    BwReplayCellBuildConfig::DropHead(config) => config.get_trace(),
                    BwReplayCellBuildConfig::DropTail(config) => config.get_trace(),
                    BwReplayCellBuildConfig::Infinite(config) => config.get_trace(),
                }
                .unwrap();

                for trace_point in expand_bw_trace(trace.as_mut(), Duration::ZERO, length) {
                    add_change_point(
                        trace_point.start_time,
                        cell_id,
                        express_bandwidth(trace_point.value),
                    )
                }
                BW_TRACE_SUFFIX
            }
            _ => {
                warn!("Cell {:?} not supported for generate combined trace", name);
                continue;
            }
        };
        name.push_str(column_suffix);
        cells_to_report.push(name);
    }

    let mut csv_writer = csv::Writer::from_writer(output);

    csv_writer
        .write_record(
            iter::once("start_time_secs")
                .chain(cells_to_report.iter().map(String::as_str))
                .chain(iter::once("duration_secs")),
        )
        .map_err(|e| Custom(format!("{:?}", e)))?;

    let mut current_state = vec![String::new(); cells_to_report.len()];

    while let Some((start_time, changes)) = change_points.pop_first() {
        let end_time = change_points
            .first_key_value()
            .map(|(&next_start, _)| next_start)
            .unwrap_or(length);

        for (cell_id, change_to) in changes {
            current_state[cell_id] = change_to;
        }
        csv_writer
            .write_record(
                iter::once(&express_time(start_time)) // start_time_secs
                    .chain(current_state.iter()) // cells
                    .chain(iter::once(&express_time(end_time - start_time))), // duration
            )
            .map_err(|e| Custom(format!("{:?}", e)))?;
    }

    Ok(())
}
