use clap::{Arg, ArgAction, Command};
use log::{error, info};
use mimalloc::MiMalloc;
use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use iptoasn_webservice::asns::Asns;
use iptoasn_webservice::DEFAULT_DB_URL;

const DEFAULT_SERVER_URL: &str = match option_env!("IPTOASN_SERVER_URL") {
    Some(url) => url,
    None => "http://127.0.0.1:53661",
};

#[tokio::main]
async fn main() {
    env_logger::init();

    let matches = Command::new("iptoasn")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Sven Mäder <maeder@phys.ethz.ch>")
        .about("Annotate IP addresses with ASN info using in-memory database. Subcommands query the iptoasn webservice")
        // Global switches for HTTP mode
        .arg(
            Arg::new("server")
                .long("server")
                .value_name("url")
                .help("Base URL of iptoasn webservice")
                .env("IPTOASN_SERVER_URL")
                .default_value(DEFAULT_SERVER_URL),
        )
        .arg(
            Arg::new("json")
                .short('j')
                .long("json")
                .help("Use JSON format for output of subcommands (Accept: application/json)")
                .action(ArgAction::SetTrue),
        )
        // Subcommands for HTTP API usage
        .subcommand(
            Command::new("ip")
                .about("Lookup IP via webservice")
                .arg(
                    Arg::new("ip")
                        .value_name("ip")
                        .help("IP address (optional). If omitted, lookup requester IP")
                        .required(false),
                ),
        )
        .subcommand(
            Command::new("ips")
                .about("Bulk IP lookup via webservice; reads IPs from file or stdin. Input can be text/plain or JSON (auto-detected).")
                .arg(
                    Arg::new("file")
                        .value_name("file")
                        .help("Path to file with IPs; if not set, reads from stdin")
                        .required(false),
                ),
        )
        .subcommand(
            Command::new("asn")
                .about("AS number lookup via webservice, or subcommands")
                .arg(
                    Arg::new("asn")
                        .value_name("as number")
                        .help("AS number (e.g., 15169 or AS15169)")
                        .required(false),
                )
                .subcommand(
                    Command::new("subnets").about("List subnets of an AS").arg(
                        Arg::new("asn")
                            .value_name("as number")
                            .help("AS number (e.g., 15169 or AS15169)")
                            .required(true),
                    ),
                ),
        )
        .subcommand(Command::new("asns").about("List all AS numbers via webservice"))
        // Original annotate-mode arguments (used when no HTTP subcommands are present)
        .arg(
            Arg::new("db_url")
                .short('u')
                .long("dburl")
                .value_name("db_url")
                .help("URL to download the in-memory database")
                .env("IPTOASN_DB_URL")
                .default_value(DEFAULT_DB_URL),
        )
        .arg(
            Arg::new("cache_file")
                .short('c')
                .long("cache-file")
                .value_name("path")
                .help("Override path to cache file [env: $XDG_CACHE_HOME/iptoasn/] [default: ~/.cache/iptoasn/]"),
        )
        .arg(
            Arg::new("input")
                .short('i')
                .long("input")
                .value_name("path")
                .help("Path to input file (defaults to stdin)"),
        )
        .arg(
            Arg::new("description")
                .short('d')
                .long("description")
                .help("Include AS description in annotations")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("line_buffered")
                .short('l')
                .long("line-buffered")
                .help("Flush each output line immediately when reading from stdin")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("as_markers")
                .short('m')
                .long("as-markers")
                .value_name("pair")
                .help("Two characters: opening and closing marker for AS info (e.g., [] or <>)")
                .default_value("[]"),
        )
        .arg(
            Arg::new("as_sep")
                .short('s')
                .long("as-sep")
                .value_name("str")
                .help("Delimiter between AS info fields")
                .default_value(", "),
        )
        .get_matches();

    let server = matches.get_one::<String>("server").unwrap().to_string();
    let use_json = matches.get_flag("json");

    // If an HTTP API subcommand is used, run HTTP mode and exit
    if let Some(sub_m) = matches.subcommand_matches("ip") {
        let ip_opt = sub_m.get_one::<String>("ip").cloned();
        if let Err(code) = http_lookup_ip(&server, use_json, ip_opt.as_deref()).await {
            std::process::exit(code);
        }
        return;
    }
    if let Some(sub_m) = matches.subcommand_matches("ips") {
        let file_opt = sub_m.get_one::<String>("file").cloned();
        if let Err(code) = http_bulk_ips(&server, use_json, file_opt.as_deref()).await {
            std::process::exit(code);
        }
        return;
    }
    if matches.subcommand_matches("asns").is_some() {
        if let Err(code) = http_get_simple(&server, use_json, "/v1/as/ns").await {
            std::process::exit(code);
        }
        return;
    }
    if let Some(asn_m) = matches.subcommand_matches("asn") {
        if let Some(subnets_m) = asn_m.subcommand_matches("subnets") {
            let asn = subnets_m.get_one::<String>("asn").unwrap();
            let path = format!("/v1/as/n/{}/subnets", asn);
            if let Err(code) = http_get_simple(&server, use_json, &path).await {
                std::process::exit(code);
            }
            return;
        }
        if let Some(asn) = asn_m.get_one::<String>("asn") {
            let path = format!("/v1/as/n/{}", asn);
            if let Err(code) = http_get_simple(&server, use_json, &path).await {
                std::process::exit(code);
            }
            return;
        } else {
            eprintln!("Missing AS number. Usage: iptoasn asn <AS123|123> or iptoasn asn subnets <AS123|123>");
            std::process::exit(2);
        }
    }

    // Otherwise, run original annotate mode
    if let Err(code) = annotate_mode(&matches).await {
        std::process::exit(code);
    }
}

fn join_url(base: &str, path: &str) -> String {
    let b = base.trim_end_matches('/');
    let p = path.trim_start_matches('/');
    format!("{}/{}", b, p)
}

fn print_with_trailing_newline(s: &str) {
    if s.ends_with('\n') {
        print!("{}", s);
    } else {
        println!("{}", s);
    }
}

async fn http_lookup_ip(server: &str, use_json: bool, ip: Option<&str>) -> Result<(), i32> {
    let client = reqwest::Client::new();
    let accept = if use_json {
        "application/json"
    } else {
        "text/plain"
    };

    let path = match ip {
        Some(ip_s) => format!("/v1/as/ip/{}", ip_s),
        None => "/v1/as/ip".to_string(),
    };
    let url = join_url(server, &path);
    match client.get(&url).header(ACCEPT, accept).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                eprintln!("{}", body);
                return Err(1);
            }
            print_with_trailing_newline(&body);
            Ok(())
        }
        Err(e) => {
            eprintln!("Request failed: {}", e);
            Err(1)
        }
    }
}

async fn http_get_simple(server: &str, use_json: bool, path: &str) -> Result<(), i32> {
    let client = reqwest::Client::new();
    let accept = if use_json {
        "application/json"
    } else {
        "text/plain"
    };
    let url = join_url(server, path);
    match client.get(&url).header(ACCEPT, accept).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                eprintln!("{}", body);
                return Err(1);
            }
            print_with_trailing_newline(&body);
            Ok(())
        }
        Err(e) => {
            eprintln!("Request failed: {}", e);
            Err(1)
        }
    }
}

// Bulk IP PUT with auto-detected input content-type; output controlled by --json via Accept
async fn http_bulk_ips(server: &str, use_json: bool, file: Option<&str>) -> Result<(), i32> {
    let client = reqwest::Client::new();
    let accept = if use_json {
        "application/json"
    } else {
        "text/plain"
    };
    let url = join_url(server, "/v1/as/ips");

    // Read input (file or stdin) as-is
    let text = if let Some(path) = file {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to read file {}: {}", path, e);
                return Err(2);
            }
        }
    } else {
        let mut s = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut s) {
            eprintln!("Failed to read stdin: {}", e);
            return Err(2);
        }
        s
    };

    // Auto-detect JSON input for this endpoint; otherwise send text/plain
    let content_type = if text.trim_start().starts_with('[') {
        "application/json"
    } else {
        "text/plain"
    };

    match client
        .put(&url)
        .header(ACCEPT, accept)
        .header(CONTENT_TYPE, content_type)
        .body(text)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                eprintln!("{}", body);
                return Err(1);
            }
            print_with_trailing_newline(&body);
            Ok(())
        }
        Err(e) => {
            eprintln!("Request failed: {}", e);
            Err(1)
        }
    }
}

async fn annotate_mode(matches: &clap::ArgMatches) -> Result<(), i32> {
    let db_url = matches.get_one::<String>("db_url").unwrap();
    let include_description = matches.get_flag("description");
    let input_path = matches.get_one::<String>("input").map(String::as_str);
    let line_buffered = matches.get_flag("line_buffered");
    let cache_file: Option<PathBuf> = matches.get_one::<String>("cache_file").map(PathBuf::from);

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
            return Err(2);
        }
    };
    let as_sep = matches.get_one::<String>("as_sep").unwrap();

    // Create HTTP client once if URL is HTTP/HTTPS (for DB download)
    let http_client = if db_url.starts_with("http://") || db_url.starts_with("https://") {
        Some(reqwest::Client::new())
    } else {
        None
    };

    // Load ASN database
    let asns = match get_asns(db_url, http_client.as_ref(), cache_file.clone()).await {
        Ok(asns) => Arc::new(asns),
        Err(e) => {
            error!("Failed to load initial database: {e}");
            error!("Application cannot start without initial data");
            return Err(1);
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
                    return Err(1);
                }
            };
            Box::new(BufReader::new(file))
        }
        None => Box::new(BufReader::new(io::stdin())),
    };

    // Combined IP regex:
    //  - ip4: standard dotted IPv4
    //  - mapped: the IPv4-mapped IPv6 prefix "::ffff:" (only the prefix; we leave the following IPv4
    //            to be matched by the IPv4 branch later in the same pass)
    //  - ip6: IPv6 token with custom boundaries (excluding "::ffff:..." by virtue of the 'mapped' alt)
    let re_ip = Regex::new(
        r"(?x)
        # 1) IPv4 dotted-quad
        \b (?P<ip4> (?:\d{1,3}\.){3}\d{1,3} ) \b
        |
        # 2) IPv4-mapped IPv6 prefix '::ffff:' (do not consume dotted-quad that follows)
        (?P<pre_mapped> ^ | [^0-9A-Fa-f:] )
        (?P<mapped> :: [Ff]{4} : )
        |
        # 3) IPv6 (preserve surrounding delimiters)
        (?P<pre> ^ | [^0-9A-Fa-f:] )
        (?P<ip6> (?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4} | :: )
        (?P<post> [^0-9A-Fa-f:] | $ )
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
        let line = match line_res {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to read line: {}", e);
                return Err(1);
            }
        };

        // Single-pass replacement handling IPv4, IPv6, and IPv4-mapped IPv6 ::ffff: prefix
        let line = re_ip
            .replace_all(&line, |caps: &regex::Captures| {
                // IPv4
                if let Some(m) = caps.name("ip4") {
                    return annotate_ip_token(
                        m.as_str(),
                        include_description,
                        &asns_arc,
                        &mut cache,
                        &as_open,
                        &as_close,
                        as_sep,
                    );
                }

                // IPv4-mapped IPv6 prefix ::ffff: (return unchanged so that the following IPv4
                // can be matched and annotated by the IPv4 branch in this same pass)
                if let Some(m) = caps.name("mapped") {
                    let pre = caps.name("pre_mapped").map(|m| m.as_str()).unwrap_or("");
                    return format!("{}{}", pre, m.as_str());
                }

                // IPv6 (preserve pre/post)
                if let Some(m) = caps.name("ip6") {
                    let pre = caps.name("pre").map(|m| m.as_str()).unwrap_or("");
                    let post = caps.name("post").map(|m| m.as_str()).unwrap_or("");
                    return format!(
                        "{}{}{}",
                        pre,
                        annotate_ip_token(
                            m.as_str(),
                            include_description,
                            &asns_arc,
                            &mut cache,
                            &as_open,
                            &as_close,
                            as_sep
                        ),
                        post
                    );
                }

                // Fallback: shouldn't happen, return original match
                caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string()
            })
            .to_string();

        if let Err(e) = writeln!(stdout, "{}", line) {
            error!("Failed to write output: {}", e);
            return Err(1);
        }
    }

    if let Err(e) = stdout.flush() {
        error!("Failed to flush output: {}", e);
        return Err(1);
    }

    Ok(())
}

async fn get_asns(
    db_url: &str,
    http_client: Option<&reqwest::Client>,
    cache_file: Option<PathBuf>,
) -> Result<Asns, &'static str> {
    info!("Retrieving ASNs");
    let asns = Asns::new(db_url, http_client, cache_file)
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
        s.push_str("AS0");
        s.push_str(as_sep);
        s.push_str("None");
        if include_description {
            s.push_str(as_sep);
            s.push_str("Not announced");
        }
        s.push_str(as_close);
        s
    };

    cache.insert((ip_s.to_string(), include_description), Some(annot.clone()));
    annot
}
