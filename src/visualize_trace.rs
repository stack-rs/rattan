use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    iter,
    time::Duration,
};

use human_bandwidth::format_bandwidth;
use human_time::human_time;
use netem_trace::{
    series::{expand_bw_trace, expand_delay_trace, expand_loss_trace},
    Bandwidth, LossPattern,
};
use rattan_core::{
    cells::Packet,
    config::{BwCellBuildConfig, BwReplayCellBuildConfig, CellBuildConfig},
    error::{Result, VisualizeTraceError as Error},
};
use serde::{Deserialize, Serialize};
use serde_json::{to_value, to_writer, to_writer_pretty, Map, Value};

// Define output format of the trace logical timestamp in the .csv file here
fn express_time(time: Duration) -> String {
    format!("{:?}", time.as_secs_f64())
}

const STRAT_TIME: &str = "start_time_secs";
const DURATION_TIME: &str = "duration_secs";

const LOSS_TRACE_SUFFIX: &str = "_loss_pattern";
const BW_TRACE_SUFFIX: &str = "_bandwidth";
const DELAY_TRACE_SUFFIX: &str = "_delay";

#[derive(Clone, Copy, Debug, clap::ValueEnum, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum OutputMode {
    #[default]
    Csv,
    Tsv,
    Json,
    HumanJson,
}

impl OutputMode {
    pub fn get_extension_name(&self) -> &'static str {
        match self {
            OutputMode::Csv => "csv",
            OutputMode::Tsv => "tsv",
            OutputMode::Json => "json",
            OutputMode::HumanJson => "json",
        }
    }
}

#[derive(Clone, Debug)]
enum TracePoint {
    Delay(Duration),
    Bandwidth(Bandwidth),
    Loss(LossPattern),
}

impl TracePoint {
    fn display(&self) -> String {
        match self {
            TracePoint::Bandwidth(bw) => format_bandwidth(*bw).to_string(),
            TracePoint::Delay(delay) => human_time(*delay),
            TracePoint::Loss(loss) => loss
                .iter()
                .map(f64::to_string)
                .collect::<Vec<_>>()
                .join(";"),
        }
    }

    fn to_json(&self, human_readable: bool) -> Option<Value> {
        match (self, human_readable) {
            (TracePoint::Bandwidth(bw), false) => to_value(bw),
            (TracePoint::Bandwidth(bw), true) => to_value(format_bandwidth(*bw).to_string()),
            (TracePoint::Delay(delay), false) => to_value(delay),
            (TracePoint::Delay(delay), true) => to_value(human_time(*delay)),
            (TracePoint::Loss(loss), _) => to_value(loss),
        }
        .ok()
    }
}

#[derive(Default)]
struct TraceRecord {
    cell_names: Vec<String>,
    change_points: BTreeMap<Duration, Vec<(usize, TracePoint)>>,
}

impl TraceRecord {
    pub fn add_cell(
        &mut self,
        mut name: String,
        suffix: &str,
        changes: impl Iterator<Item = (Duration, TracePoint)>,
    ) {
        let cell_id = self.cell_names.len();
        name.push_str(suffix);
        self.cell_names.push(name);
        // XXX: For very long traces or high-frequency changes, this might consume a significant amount
        // of memory. If this becomes an issue, a streaming approach (using a k-way merge of iterators
        // from the traces of different cells) could be considered.
        for (start_time, point) in changes {
            self.change_points
                .entry(start_time)
                .or_default()
                .push((cell_id, point));
        }
    }
}

struct VisualizeTraceBuilder {
    inner: TraceRecord,
    current: Vec<Option<TracePoint>>,
    current_time: Option<Duration>,
}

impl VisualizeTraceBuilder {
    pub fn new(inner: TraceRecord) -> Self {
        let current = vec![None; inner.cell_names.len()];
        Self {
            inner,
            current,
            current_time: None,
        }
    }

    fn next(&mut self) -> Option<Duration> {
        let (start, changes) = self.inner.change_points.pop_first()?;
        self.current_time = start.into();
        for (cell_id, change_to) in changes {
            self.current[cell_id] = change_to.into();
        }
        self.inner
            .change_points
            .first_key_value()
            .map(|(&next_start, _)| next_start)
    }

    fn write_as_csv(
        mut self,
        end_time: Duration,
        mut csv_writer: csv::Writer<impl Write>,
    ) -> Result<()> {
        // Write column names
        csv_writer
            .write_record(
                iter::once(STRAT_TIME)
                    .chain(iter::once(DURATION_TIME))
                    .chain(self.inner.cell_names.iter().map(String::as_str)),
            )
            .map_err(Error::CsvError)?;

        loop {
            // Write by rows
            let end_time = self.next().unwrap_or(end_time);
            let Some(start_time) = self.current_time.take() else {
                return Ok(());
            };
            let times = [start_time, end_time - start_time];

            let row = times.into_iter().map(express_time).chain(
                self.current
                    .iter()
                    .map(|trace| trace.as_ref().map(TracePoint::display).unwrap_or_default()),
            );

            csv_writer.write_record(row).map_err(Error::CsvError)?;
        }
    }

    fn write_as_json(mut self, end_time: Duration, human_readable: bool) -> Result<Value> {
        let mut data = vec![];

        loop {
            // Write by rows
            let end_time = self.next().unwrap_or(end_time);
            let Some(start_time) = self.current_time.take() else {
                break;
            };

            let mut row = Map::new();

            if human_readable {
                row.insert(STRAT_TIME.to_string(), to_value(human_time(start_time))?);
                row.insert(
                    DURATION_TIME.to_string(),
                    to_value(human_time(end_time - start_time))?,
                );
            } else {
                row.insert(STRAT_TIME.to_string(), to_value(start_time)?);
                row.insert(DURATION_TIME.to_string(), to_value(end_time - start_time)?);
            }
            for (key, value) in self.inner.cell_names.iter().zip(
                self.current
                    .iter()
                    .map(|e| e.as_ref().and_then(|v| v.to_json(human_readable))),
            ) {
                if let Some(value) = value {
                    row.insert(key.clone(), value);
                }
            }
            data.push(Value::Object(row));
        }
        Ok(Value::Array(data))
    }

    pub fn write(self, output: impl Write, mode: OutputMode, end_time: Duration) -> Result<()> {
        match mode {
            OutputMode::Csv => {
                let writer = csv::Writer::from_writer(output);
                self.write_as_csv(end_time, writer)?;
            }
            OutputMode::Tsv => {
                let writer = csv::WriterBuilder::new()
                    .delimiter(b'\t')
                    .from_writer(output);
                self.write_as_csv(end_time, writer)?;
            }
            OutputMode::Json => {
                let value = self.write_as_json(end_time, false)?;
                to_writer(output, &value)?
            }
            OutputMode::HumanJson => {
                let value = self.write_as_json(end_time, true)?;
                to_writer_pretty(output, &value)?
            }
        }
        Ok(())
    }
}

pub fn write_visualize_trace<P: Packet>(
    output_mode: OutputMode,
    output: impl Write,
    cells: HashMap<String, CellBuildConfig<P>>,
    start: Option<Duration>,
    end_time: Duration,
) -> Result<()> {
    let start = start.unwrap_or_default();

    let mut trace_record = TraceRecord::default();

    for (name, cell) in cells {
        match cell {
            CellBuildConfig::Loss(config) => trace_record.add_cell(
                name,
                LOSS_TRACE_SUFFIX,
                iter::once((start, TracePoint::Loss(config.pattern))),
            ),
            CellBuildConfig::Delay(config) => trace_record.add_cell(
                name,
                DELAY_TRACE_SUFFIX,
                iter::once((start, TracePoint::Delay(config.delay))),
            ),
            CellBuildConfig::Bw(config) => {
                let bandwidth = match config {
                    BwCellBuildConfig::CoDel(config) => config.bandwidth,
                    BwCellBuildConfig::Infinite(config) => config.bandwidth,
                    BwCellBuildConfig::DropHead(config) => config.bandwidth,
                    BwCellBuildConfig::DropTail(config) => config.bandwidth,
                }
                .unwrap_or(Bandwidth::from_bps(u64::MAX));
                trace_record.add_cell(
                    name,
                    BW_TRACE_SUFFIX,
                    iter::once((start, TracePoint::Bandwidth(bandwidth))),
                );
            }

            CellBuildConfig::LossReplay(config) => {
                let mut trace = config.get_trace()?;
                let trace_points = expand_loss_trace(trace.as_mut(), start, end_time)
                    .into_iter()
                    .map(|point| (point.start_time, TracePoint::Loss(point.value)));
                trace_record.add_cell(name, LOSS_TRACE_SUFFIX, trace_points);
            }

            CellBuildConfig::DelayReplay(config) => {
                let mut trace = config.get_trace()?;
                let trace_points = expand_delay_trace(trace.as_mut(), start, end_time)
                    .into_iter()
                    .map(|point| (point.start_time, TracePoint::Delay(point.value)));
                trace_record.add_cell(name, DELAY_TRACE_SUFFIX, trace_points);
            }

            CellBuildConfig::BwReplay(config) => {
                let mut trace = match config {
                    BwReplayCellBuildConfig::CoDel(config) => config.get_trace(),
                    BwReplayCellBuildConfig::DropHead(config) => config.get_trace(),
                    BwReplayCellBuildConfig::DropTail(config) => config.get_trace(),
                    BwReplayCellBuildConfig::Infinite(config) => config.get_trace(),
                }?;

                let trace_points = expand_bw_trace(trace.as_mut(), start, end_time)
                    .into_iter()
                    .map(|point| (point.start_time, TracePoint::Bandwidth(point.value)));
                trace_record.add_cell(name, BW_TRACE_SUFFIX, trace_points);
            }
            _ => {
                tracing::warn!("Cell {:?} not supported for generate combined trace", name);
                continue;
            }
        };
    }

    VisualizeTraceBuilder::new(trace_record).write(output, output_mode, end_time)
}
