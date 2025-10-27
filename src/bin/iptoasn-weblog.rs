use clap::{Arg, Command};
use log::{error, info};
use mimalloc::MiMalloc;
use regex::Regex;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use iptoasn_webservice::asns::Asns;

#[tokio::main]
async fn main() {
    env_logger::init();

    let matches = Command::new("iptoasn-weblog")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Sven Mäder <maeder@phys.ethz.ch>")
        .about("Annotate Apache/nginx logs with ASN info for client IPs")
        .arg(
            Arg::new("db_url")
                .short('u')
                .long("dburl")
                .value_name("db_url")
                .help("URL of the database")
                .default_value("https://iptoasn.com/data/ip2asn-combined.tsv.gz"),
        )
        .arg(
            Arg::new("input")
                .short('i')
                .long("input")
                .value_name("path")
                .help("Path to input log file (defaults to stdin)"),
        )
        .arg(
            Arg::new("description")
                .short('d')
                .long("description")
                .help("Include AS description in annotations")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("line_buffered")
                .short('l')
                .long("line-buffered")
                .help("Flush each output line immediately when reading from stdin")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("as_markers")
                .long("as-markers")
                .value_name("pair")
                .help("Two characters: opening and closing marker for AS info (e.g., [] or <>)")
                .default_value("[]"),
        )
        .arg(
            Arg::new("as_sep")
                .long("as-sep")
                .value_name("str")
                .help("Delimiter between AS info fields")
                .default_value(", "),
        )
        .get_matches();

    let db_url = matches.get_one::<String>("db_url").unwrap();
    let include_description = matches.get_flag("description");
    let input_path = matches.get_one::<String>("input").map(String::as_str);
    let line_buffered = matches.get_flag("line_buffered");

    // Parse AS markers (must be exactly two Unicode characters)
    let as_markers = matches.get_one::<String>("as_markers").unwrap();
    let mut chs = as_markers.chars();
    let (as_open, as_close) = match (chs.next(), chs.next(), chs.next()) {
        (Some(o), Some(c), None) => (o.to_string(), c.to_string()),
        _ => {
            error!(
                "--as-markers must be exactly two characters, e.g., \"[]\" or \"<>\", got: {}",
                as_markers
            );
            std::process::exit(2);
        }
    };
    let as_sep = matches.get_one::<String>("as_sep").unwrap();

    // Create HTTP client once if URL is HTTP/HTTPS
    let http_client = if db_url.starts_with("http://") || db_url.starts_with("https://") {
        Some(reqwest::Client::new())
    } else {
        None
    };

    // Load ASN database
    let asns = match get_asns(db_url, http_client.as_ref()).await {
        Ok(asns) => Arc::new(asns),
        Err(e) => {
            error!("Failed to load initial database: {e}");
            error!("Application cannot start without initial data");
            std::process::exit(1);
        }
    };
    let asns_arc = Arc::new(RwLock::new(asns));

    // Prepare input reader (file or stdin)
    let reader: Box<dyn BufRead> = match input_path {
        Some(path) => {
            let file = match File::open(path) {
                Ok(f) => f,
                Err(e) => {
                    error!("Failed to open input file {}: {}", path, e);
                    std::process::exit(1);
                }
            };
            Box::new(BufReader::new(file))
        }
        None => Box::new(BufReader::new(io::stdin())),
    };

    // Precompile IP-matching regex
    let re_ipv4 = Regex::new(r"\b(?P<ip>(?:\d{1,3}\.){3}\d{1,3})\b").unwrap();

    // IPv6 matching that doesn't rely on \b and preserves delimiters.
    // Modified to ignore IPv6 addresses starting with ::ffff (IPv4-mapped IPv6).
    // Those are left untouched here so that the IPv4 regex handles the embedded IPv4.
    let re_ipv6 = Regex::new(
        r"(?x)
          (?P<pre>^|[^0-9A-Fa-f:])
          (?:
              (?P<skip>::[Ff]{4}:[0-9A-Fa-f:.]+)           # IPv4-mapped (::ffff:...)
            |
              (?P<ip>(?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}|::)
          )
          (?P<post>[^0-9A-Fa-f:]|$)
        ",
    )
    .unwrap();

    // Choose output writer: line-buffered for stdin when requested, else buffered
    let stdout_raw = io::stdout();
    let mut stdout: Box<dyn Write> = if line_buffered && input_path.is_none() {
        Box::new(io::LineWriter::new(stdout_raw))
    } else {
        Box::new(io::BufWriter::new(stdout_raw))
    };

    // Cache to avoid repeated lookups across the whole run
    let mut cache: HashMap<(String, bool), Option<String>> = HashMap::new();

    for line_res in reader.lines() {
        let mut line = match line_res {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to read line: {}", e);
                std::process::exit(1);
            }
        };

        // Replace IPv4 occurrences
        line = re_ipv4
            .replace_all(&line, |caps: &regex::Captures| {
                let ip_s = caps.name("ip").unwrap().as_str();
                annotate_ip_token(
                    ip_s,
                    include_description,
                    &asns_arc,
                    &mut cache,
                    &as_open,
                    &as_close,
                    as_sep,
                )
            })
            .to_string();

        // Replace IPv6 occurrences (preserving surrounding delimiters),
        // but ignore IPv4-mapped IPv6 (::ffff:...)
        line = re_ipv6
            .replace_all(&line, |caps: &regex::Captures| {
                let pre = caps.name("pre").map(|m| m.as_str()).unwrap_or("");
                let post = caps.name("post").map(|m| m.as_str()).unwrap_or("");

                if let Some(skip) = caps.name("skip") {
                    // Leave ::ffff:... as-is; IPv4 part already handled by IPv4 regex.
                    return format!("{}{}{}", pre, skip.as_str(), post);
                }

                let ip_s = caps.name("ip").unwrap().as_str();
                format!(
                    "{}{}{}",
                    pre,
                    annotate_ip_token(
                        ip_s,
                        include_description,
                        &asns_arc,
                        &mut cache,
                        &as_open,
                        &as_close,
                        as_sep
                    ),
                    post
                )
            })
            .to_string();

        if let Err(e) = writeln!(stdout, "{}", line) {
            error!("Failed to write output: {}", e);
            std::process::exit(1);
        }
    }

    if let Err(e) = stdout.flush() {
        error!("Failed to flush output: {}", e);
        std::process::exit(1);
    }
}

async fn get_asns(
    db_url: &str,
    http_client: Option<&reqwest::Client>,
) -> Result<Asns, &'static str> {
    info!("Retrieving ASNs");
    let asns = Asns::new(db_url, http_client)
        .await
        .map_err(|_| "ASNs load failed")?;
    info!("ASNs loaded");
    Ok(asns)
}

fn annotate_ip_token(
    ip_s: &str,
    include_description: bool,
    asns_arc: &Arc<RwLock<Arc<Asns>>>,
    cache: &mut HashMap<(String, bool), Option<String>>,
    as_open: &str,
    as_close: &str,
    as_sep: &str,
) -> String {
    if let Some(cached) = cache.get(&(ip_s.to_string(), include_description)) {
        return match cached {
            Some(ann) => ann.clone(),
            None => ip_s.to_string(),
        };
    }

    let ip = match IpAddr::from_str(ip_s) {
        Ok(ip) => ip,
        Err(_) => {
            // Not a valid IP token; leave unchanged
            cache.insert((ip_s.to_string(), include_description), None);
            return ip_s.to_string();
        }
    };

    let asns = asns_arc.read().unwrap().clone();

    let annot = if let Some(found) = asns.lookup_by_ip(ip) {
        let mut s = String::new();
        s.push_str(ip_s);
        s.push(' ');
        s.push_str(as_open);
        s.push_str("AS");
        s.push_str(&found.number.to_string());
        s.push_str(as_sep);
        s.push_str(&found.country);
        if include_description {
            s.push_str(as_sep);
            s.push_str(&found.description);
        }
        s.push_str(as_close);
        s
    } else {
        // No ASN found (local/private or unrouted)
        let mut s = String::new();
        s.push_str(ip_s);
        s.push(' ');
        s.push_str(as_open);
        s.push_str("NA");
        s.push_str(as_sep);
        s.push_str("--");
        if include_description {
            s.push_str(as_sep);
            s.push_str("local or unknown");
        }
        s.push_str(as_close);
        s
    };

    cache.insert((ip_s.to_string(), include_description), Some(annot.clone()));
    annot
}
