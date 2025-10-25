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
        .get_matches();

    let db_url = matches.get_one::<String>("db_url").unwrap();
    let include_description = matches.get_flag("description");
    let input_path = matches.get_one::<String>("input").map(String::as_str);
    let line_buffered = matches.get_flag("line_buffered");

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

    // IPv6: capture optional 1-char pre-delimiter (or start), the IPv6 token, and the post-delimiter (or end).
    // This avoids relying on \b, which fails when the address ends with ':' (e.g., '::').
    // We keep the delimiters in the match and re-insert them in the replacement to preserve text.
    let re_ipv6 = Regex::new(
        r"(?P<pre>^|[^0-9A-Fa-f:])(?P<ip>(?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}|::)(?P<post>[^0-9A-Fa-f:]|$)"
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
                annotate_ip_token(ip_s, include_description, &asns_arc, &mut cache)
            })
            .to_string();

        // Replace IPv6 occurrences (preserving surrounding delimiters)
        line = re_ipv6
            .replace_all(&line, |caps: &regex::Captures| {
                let pre = caps.name("pre").map(|m| m.as_str()).unwrap_or("");
                let ip_s = caps.name("ip").unwrap().as_str();
                let post = caps.name("post").map(|m| m.as_str()).unwrap_or("");
                format!(
                    "{}{}{}",
                    pre,
                    annotate_ip_token(ip_s, include_description, &asns_arc, &mut cache),
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
    let asns = Asns::new(db_url, http_client).await.map_err(|_| "ASNs load failed")?;
    info!("ASNs loaded");
    Ok(asns)
}

fn annotate_ip_token(
    ip_s: &str,
    include_description: bool,
    asns_arc: &Arc<RwLock<Arc<Asns>>>,
    cache: &mut HashMap<(String, bool), Option<String>>,
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
        let mut s = format!("{} [AS{}, {}", ip_s, found.number, found.country);
        if include_description {
            s.push_str(", ");
            s.push_str(&found.description);
        }
        s.push(']');
        s
    } else {
        // No ASN found (local/private or unrouted)
        let mut s = format!("{} [NA, --", ip_s);
        if include_description {
            s.push_str(", local or unknown");
        }
        s.push(']');
        s
    };

    cache.insert((ip_s.to_string(), include_description), Some(annot.clone()));
    annot
}
