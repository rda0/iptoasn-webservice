use chrono::{
    DateTime, Datelike, Local, NaiveDateTime, TimeZone,
};
use rayon::prelude::*;
use regex::Regex;
use std::{
    collections::HashMap,
    env,
    io::{self, BufRead, BufReader},
    process,
};

type Key = (u32, String);
type Event = (f64, Vec<Key>);

const FLAME: [(f64, (u8, u8, u8)); 9] = [
    (0.00, (0, 0, 0)),
    (0.08, (35, 0, 0)),
    (0.20, (100, 0, 0)),
    (0.36, (180, 0, 0)),
    (0.52, (255, 35, 0)),
    (0.68, (255, 105, 0)),
    (0.82, (255, 190, 0)),
    (0.93, (255, 235, 80)),
    (1.00, (255, 255, 255)),
];

struct Config {
    no_color: bool,
    show_all: bool,
    bins: usize,
    threads: usize,
}

fn print_help(program: &str) {
    println!(
        "\
Usage: {program} [OPTIONS]

Read timestamped AS annotations from stdin and print terminal heatmap.

Options:
    -t, --threads N       Worker threads. Default: one quarter logical CPUs, capped at 8.
        --bins N          Time bins. Default: terminal graph width.
        --no-color         Use ASCII density instead 256-color heatmap.
        --all              Show all AS rows, beyond terminal height.
    -h, --help             Show this help.

Examples:
    cat input.log | {program}
    cat input.log | {program} --all
    cat input.log | {program} -t 8 --bins 200
"
    );
}

fn parse_args() -> Config {
    let args: Vec<String> = env::args().collect();
    let program = args
        .first()
        .map(String::as_str)
        .unwrap_or("asheat");

    let mut no_color = false;
    let mut show_all = false;
    let mut bins = 0usize;
    let mut threads = 0usize;
    let mut index = 1;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print_help(program);
                process::exit(0);
            }

            "--no-color" => {
                no_color = true;
            }

            "--all" => {
                show_all = true;
            }

            "--bins" => {
                index += 1;

                if index >= args.len() {
                    eprintln!("--bins requires number");
                    process::exit(2);
                }

                bins = args[index].parse().unwrap_or_else(|_| {
                    eprintln!("Invalid bin count: {}", args[index]);
                    process::exit(2);
                });
            }

            "--threads" | "-t" => {
                index += 1;

                if index >= args.len() {
                    eprintln!("{} requires number", args[index - 1]);
                    process::exit(2);
                }

                threads = args[index].parse().unwrap_or_else(|_| {
                    eprintln!("Invalid thread count: {}", args[index]);
                    process::exit(2);
                });
            }

            value if value.starts_with("--bins=") => {
                bins = value[7..].parse().unwrap_or_else(|_| {
                    eprintln!("Invalid bin count: {}", &value[7..]);
                    process::exit(2);
                });
            }

            value if value.starts_with("--threads=") => {
                threads = value[10..].parse().unwrap_or_else(|_| {
                    eprintln!("Invalid thread count: {}", &value[10..]);
                    process::exit(2);
                });
            }

            value if value.starts_with("-t") && value.len() > 2 => {
                threads = value[2..].parse().unwrap_or_else(|_| {
                    eprintln!("Invalid thread count: {}", &value[2..]);
                    process::exit(2);
                });
            }

            "-d" | "-f" => {}

            value => {
                eprintln!("Unknown option: {value}");
                eprintln!("Try '{program} --help'.");
                process::exit(2);
            }
        }

        index += 1;
    }

    if threads == 0 {
        let logical = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);

        let default_threads = (logical / 4).clamp(1, 8);
        threads = default_threads;
    }

    Config {
        no_color,
        show_all,
        bins,
        threads: threads.max(1),
    }
}

fn build_date_regex() -> Regex {
    Regex::new(
        r"(?P<iso>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2}))|(?P<apache>\d{2}/[A-Za-z]{3}/\d{4}:\d{2}:\d{2}:\d{2}(?: [+-]\d{4})?)|(?P<syslog>[A-Z][a-z]{2}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2})",
    )
    .unwrap()
}

fn parse_timestamp(captures: &regex::Captures<'_>) -> Option<f64> {
    if let Some(value) = captures.name("apache") {
        let value = value.as_str();

        if let Ok(time) =
            DateTime::parse_from_str(value, "%d/%b/%Y:%H:%M:%S %z")
        {
            return Some(time.timestamp_millis() as f64 / 1000.0);
        }

        if let Ok(time) =
            NaiveDateTime::parse_from_str(value, "%d/%b/%Y:%H:%M:%S")
        {
            let local = Local.from_local_datetime(&time).single()?;

            return Some(local.timestamp_millis() as f64 / 1000.0);
        }

        return None;
    }

    if let Some(value) = captures.name("iso") {
        let value = value.as_str().replace('Z', "+00:00");

        if let Ok(time) = DateTime::parse_from_rfc3339(&value) {
            return Some(time.timestamp_millis() as f64 / 1000.0);
        }

        return None;
    }

    if let Some(value) = captures.name("syslog") {
        let year = Local::now().year();
        let value = format!("{} {}", year, value.as_str());

        if let Ok(time) =
            NaiveDateTime::parse_from_str(&value, "%Y %b %d %H:%M:%S")
        {
            let local = Local.from_local_datetime(&time).single()?;

            return Some(local.timestamp_millis() as f64 / 1000.0);
        }
    }

    None
}

fn parse_as_annotations(line: &[u8]) -> Vec<Key> {
    let mut result = Vec::new();
    let mut position = 0;

    while position + 3 <= line.len() {
        let Some(offset) = line[position..]
            .windows(3)
            .position(|window| window == b"[AS")
        else {
            break;
        };

        let start = position + offset + 3;

        let Some(comma_offset) = line[start..]
            .iter()
            .position(|&byte| byte == b',')
        else {
            break;
        };

        let asn_end = start + comma_offset;

        let asn = match std::str::from_utf8(&line[start..asn_end])
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            Some(value) => value,
            None => {
                position = start;
                continue;
            }
        };

        let description_start = asn_end + 1;

        let Some(end_offset) = line[description_start..]
            .iter()
            .position(|&byte| byte == b']')
        else {
            break;
        };

        let description_end = description_start + end_offset;

        let description = String::from_utf8_lossy(
            &line[description_start..description_end],
        )
        .trim()
        .to_owned();

        let duplicate = result.iter().any(
            |(old_asn, old_description)| {
                *old_asn == asn && *old_description == description
            },
        );

        if !duplicate {
            result.push((asn, description));
        }

        position = description_end + 1;
    }

    result
}

fn parse_line(
    line: &str,
    date_re: &Regex,
) -> Option<Event> {
    let captures = date_re.captures(line)?;

    let timestamp = parse_timestamp(&captures)?;
    let annotations = parse_as_annotations(line.as_bytes());

    if annotations.is_empty() {
        None
    } else {
        Some((timestamp, annotations))
    }
}

fn build_palette() -> Vec<(u8, u8, u8)> {
    let mut palette = vec![
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];

    let cube = [0, 95, 135, 175, 215, 255];

    for &red in &cube {
        for &green in &cube {
            for &blue in &cube {
                palette.push((red, green, blue));
            }
        }
    }

    for value in 0..24 {
        let level = 8 + value * 10;
        palette.push((level, level, level));
    }

    palette
}

fn flame_rgb(position: f64) -> (u8, u8, u8) {
    let position = position.clamp(0.0, 1.0);

    for index in 0..FLAME.len() - 1 {
        let (left_position, left) = FLAME[index];
        let (right_position, right) = FLAME[index + 1];

        if position <= right_position {
            let fraction =
                (position - left_position)
                    / (right_position - left_position);

            return (
                (left.0 as f64
                    + (right.0 as f64 - left.0 as f64) * fraction)
                    .round() as u8,
                (left.1 as f64
                    + (right.1 as f64 - left.1 as f64) * fraction)
                    .round() as u8,
                (left.2 as f64
                    + (right.2 as f64 - left.2 as f64) * fraction)
                    .round() as u8,
            );
        }
    }

    FLAME[FLAME.len() - 1].1
}

fn nearest_xterm(
    rgb: (u8, u8, u8),
    palette: &[(u8, u8, u8)],
) -> usize {
    let mut best_index = 16;
    let mut best_distance = u32::MAX;

    for index in 16..256 {
        let color = palette[index];

        let red = color.0 as i32 - rgb.0 as i32;
        let green = color.1 as i32 - rgb.1 as i32;
        let blue = color.2 as i32 - rgb.2 as i32;

        let distance =
            (red * red + green * green + blue * blue) as u32;

        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }

    best_index
}

fn heat_color(
    value: usize,
    maximum: usize,
    palette: &[(u8, u8, u8)],
) -> String {
    if value == 0 {
        return " ".to_string();
    }

    let position = if maximum <= 1 {
        1.0
    } else {
        (value as f64 + 1.0).ln()
            / (maximum as f64 + 1.0).ln()
    };

    let color = nearest_xterm(
        flame_rgb(position),
        palette,
    );

    format!("\x1b[48;5;{}m \x1b[0m", color)
}

fn heat_ascii(value: usize, maximum: usize) -> char {
    if value == 0 {
        return ' ';
    }

    let chars = b" .:-=+*#%@";

    let position = if maximum <= 1 {
        1.0
    } else {
        (value as f64 + 1.0).ln()
            / (maximum as f64 + 1.0).ln()
    };

    let index = (position * (chars.len() - 1) as f64)
        .round() as usize;

    chars[index.clamp(1, chars.len() - 1)] as char
}

fn local_time(timestamp: f64) -> chrono::DateTime<Local> {
    let seconds = timestamp.floor() as i64;

    let nanos = ((timestamp - timestamp.floor())
        * 1_000_000_000.0) as u32;

    Local
        .timestamp_opt(seconds, nanos)
        .single()
        .unwrap_or_else(Local::now)
}

fn format_time(timestamp: f64, span: f64) -> String {
    let time = local_time(timestamp);

    if span >= 86_400.0 {
        time.format("%d/%b %H:%M").to_string()
    } else if span >= 3_600.0 {
        time.format("%H:%M").to_string()
    } else if span >= 60.0 {
        time.format("%H:%M:%S").to_string()
    } else {
        time.format("%H:%M:%S%.3f").to_string()
    }
}

fn format_duration(seconds: f64) -> String {
    if seconds < 1.0 {
        format!("{:.2}s", seconds)
    } else if seconds < 60.0 {
        format!("{:.1}s", seconds)
    } else if seconds < 3_600.0 {
        format!("{:.1}m", seconds / 60.0)
    } else if seconds < 86_400.0 {
        format!("{:.1}h", seconds / 3_600.0)
    } else {
        format!("{:.1}d", seconds / 86_400.0)
    }
}

fn fit_label(asn: u32, description: &str) -> String {
    let mut label = format!("AS{} {}", asn, description);

    if label.chars().count() > 40 {
        label = label.chars().take(39).collect();
        label.push('…');
    }

    label.push_str(&" ".repeat(
        40usize.saturating_sub(label.chars().count()),
    ));

    label
}

fn terminal_size() -> (usize, usize) {
    unsafe {
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        if libc::ioctl(
            libc::STDOUT_FILENO,
            libc::TIOCGWINSZ,
            &mut size,
        ) == 0
            && size.ws_col > 0
            && size.ws_row > 0
        {
            return (
                size.ws_col as usize,
                size.ws_row as usize,
            );
        }
    }

    let columns = env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120);

    let rows = env::var("LINES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(40);

    (columns, rows)
}

fn merge_counts(
    target_counts: &mut HashMap<Key, Vec<usize>>,
    target_totals: &mut HashMap<Key, usize>,
    source_counts: HashMap<Key, Vec<usize>>,
    source_totals: HashMap<Key, usize>,
    bin_count: usize,
) {
    for (key, values) in source_counts {
        let target = target_counts
            .entry(key.clone())
            .or_insert_with(|| vec![0; bin_count]);

        for (left, right) in target.iter_mut().zip(values) {
            *left += right;
        }
    }

    for (key, total) in source_totals {
        *target_totals.entry(key).or_insert(0) += total;
    }
}

fn main() {
    let config = parse_args();

    rayon::ThreadPoolBuilder::new()
        .num_threads(config.threads)
        .build_global()
        .expect("failed creating thread pool");

    let date_re = build_date_regex();

    let stdin = io::stdin();

    let reader = BufReader::with_capacity(
        8 * 1024 * 1024,
        stdin.lock(),
    );

    let lines: Vec<String> = reader
        .lines()
        .map_while(Result::ok)
        .collect();

    let events: Vec<Event> = lines
        .par_iter()
        .filter_map(|line| parse_line(line, &date_re))
        .collect();

    if events.is_empty() {
        eprintln!("No timestamped AS annotations found.");
        process::exit(1);
    }

    let (columns, rows) = terminal_size();

    let label_width = 40;
    let graph_width =
        columns.saturating_sub(label_width + 1).max(8);

    let bin_count = if config.bins == 0 {
        graph_width
    } else {
        config.bins
    }
    .clamp(1, graph_width);

    let start = events
        .iter()
        .map(|event| event.0)
        .fold(f64::INFINITY, f64::min);

    let end = events
        .iter()
        .map(|event| event.0)
        .fold(f64::NEG_INFINITY, f64::max);

    let span = (end - start).max(1.0);
    let bin_seconds = span / bin_count as f64;

    let chunk_size = (
        events.len() / (config.threads * 4).max(1)
    )
    .max(256);

    let partials: Vec<(
        HashMap<Key, Vec<usize>>,
        HashMap<Key, usize>,
    )> = events
        .par_chunks(chunk_size)
        .map(|chunk| {
            let mut counts: HashMap<Key, Vec<usize>> =
                HashMap::new();

            let mut totals: HashMap<Key, usize> =
                HashMap::new();

            for (timestamp, annotations) in chunk {
                let position = (((timestamp - start)
                    / bin_seconds) as usize)
                    .min(bin_count - 1);

                for key in annotations {
                    counts
                        .entry(key.clone())
                        .or_insert_with(|| vec![0; bin_count])
                        [position] += 1;

                    *totals.entry(key.clone()).or_insert(0) += 1;
                }
            }

            (counts, totals)
        })
        .collect();

    let mut counts: HashMap<Key, Vec<usize>> = HashMap::new();
    let mut totals: HashMap<Key, usize> = HashMap::new();

    for (local_counts, local_totals) in partials {
        merge_counts(
            &mut counts,
            &mut totals,
            local_counts,
            local_totals,
            bin_count,
        );
    }

    let mut ordered: Vec<Key> = counts.keys().cloned().collect();

    ordered.sort_by(|left, right| {
        totals[right]
            .cmp(&totals[left])
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.cmp(&right.1))
    });

    if !config.show_all {
        let maximum_rows = rows.saturating_sub(6).max(1);
        ordered.truncate(maximum_rows);
    }

    let maximum = ordered
        .iter()
        .flat_map(|key| counts[key].iter())
        .copied()
        .max()
        .unwrap_or(1);

    let title = format!(
        "AS heatmap  events={}  range={}-{}",
        events.len(),
        format_time(start, span),
        format_time(end, span),
    );

    let prefix = " ".repeat(label_width + 1);
    let tick_step = (graph_width / 10).max(1);

    let mut ticks = vec![' '; graph_width];
    let mut labels = vec![' '; graph_width];

    for position in (0..graph_width).step_by(tick_step) {
        ticks[position] = '┬';

        let timestamp = start
            + position as f64
                / graph_width.saturating_sub(1).max(1) as f64
                * span;

        for (offset, character) in
            format_time(timestamp, span).chars().enumerate()
        {
            if position + offset < graph_width {
                labels[position + offset] = character;
            }
        }
    }

    let mut output = String::with_capacity(
        (ordered.len() + 5) * (columns + 32),
    );

    output.push_str("\x1b[1m");
    output.extend(title.chars().take(columns));
    output.push_str("\x1b[0m\n");

    output.push_str(&prefix);
    output.extend(ticks);
    output.push('\n');

    output.push_str(&prefix);
    output.extend(labels);
    output.push('\n');

    if config.no_color {
        for key in &ordered {
            output.push_str(&fit_label(key.0, &key.1));
            output.push(' ');

            for column in 0..graph_width {
                let source_column =
                    (column * bin_count / graph_width)
                        .min(bin_count - 1);

                output.push(heat_ascii(
                    counts[key][source_column],
                    maximum,
                ));
            }

            output.push('\n');
        }
    } else {
        let palette = build_palette();

        let mut heat_cells: HashMap<usize, String> =
            HashMap::new();

        for key in &ordered {
            for &value in &counts[key] {
                heat_cells.entry(value).or_insert_with(|| {
                    heat_color(value, maximum, &palette)
                });
            }
        }

        for key in &ordered {
            output.push_str(&fit_label(key.0, &key.1));
            output.push(' ');

            for column in 0..graph_width {
                let source_column =
                    (column * bin_count / graph_width)
                        .min(bin_count - 1);

                let value = counts[key][source_column];

                output.push_str(&heat_cells[&value]);
            }

            output.push('\n');
        }
    }

    let duration = format_duration(bin_seconds);

    if config.no_color {
        let legend = format!(
            "{}low: 1 req/{}  .:-=+*#%@  high: {} req/{}",
            prefix,
            duration,
            maximum,
            duration,
        );

        output.extend(legend.chars().take(columns));
        output.push('\n');
    } else {
        let colors = [
            52, 88, 124, 160, 196, 202, 208, 214, 220, 226, 255,
        ];

        output.push_str(&prefix);

        for color in colors {
            output.push_str(&format!(
                "\x1b[48;5;{}m  \x1b[0m",
                color
            ));
        }

        let legend = format!(
            " low: 1 req/{} → high: {} req/{}",
            duration,
            maximum,
            duration,
        );

        output.extend(legend.chars().take(
            columns.saturating_sub(
                label_width + 1 + colors.len() * 2,
            ),
        ));

        output.push('\n');
    }

    print!("{}", output);
}
