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
use std::sync::Arc;
use rayon::prelude::*;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use iptoasn_webservice::asns::Asns;
use iptoasn_webservice::DEFAULT_DB_URL;

const DEFAULT_SERVER_URL: &str = match option_env!("IPTOASN_SERVER_URL") {
    Some(url) => url,
    None => "http://127.0.0.1:53661",
};

const BATCH_SIZE: usize = 4096;
const LINES_PER_CHUNK: usize = 64;

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
        .subcommand(
            Command::new("country")
                .about("Country lookup via webservice, or subcommands")
                .arg(
                    Arg::new("cc")
                        .value_name("country code")
                        .help("2-letter country code (e.g., US)")
                        .required(false),
                )
                .subcommand(
                    Command::new("subnets")
                        .about("List subnets of a country (deaggregated/merged)")
                        .arg(
                            Arg::new("cc")
                                .value_name("country code")
                                .help("2-letter country code (e.g., US)")
                                .required(true),
                        ),
                ),
        )
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
        .arg(
            Arg::new("first")
                .short('f')
                .long("first")
                .value_name("n")
                .help("Only replace first N IPs per line. -f alone sets N=1. To specify N, use -f=N or --first=N. If omitted, replace all")
                .num_args(0..=1)
                .require_equals(true)
                .value_parser(clap::value_parser!(usize))
                .default_missing_value("1"),
        )
        .arg(
            Arg::new("threads")
                .short('t')
                .long("threads")
                .value_name("N")
                .help("Worker thread count. Default: one quarter logical CPUs, capped at 8")
                .value_parser(clap::value_parser!(usize)),
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
    if let Some(cc_m) = matches.subcommand_matches("country") {
        if let Some(subnets_m) = cc_m.subcommand_matches("subnets") {
            let cc = subnets_m.get_one::<String>("cc").unwrap();
            let path = format!("/v1/as/country/{}/subnets", cc);
            if let Err(code) = http_get_simple(&server, use_json, &path).await {
                std::process::exit(code);
            }
            return;
        }
        if let Some(cc) = cc_m.get_one::<String>("cc") {
            let path = format!("/v1/as/country/{}", cc);
            if let Err(code) = http_get_simple(&server, use_json, &path).await {
                std::process::exit(code);
            }
            return;
        } else {
            eprintln!("Missing country code. Usage: iptoasn country <CC> or iptoasn country subnets <CC>");
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

fn replace_ip_addresses(
    line: &str,
    re_ip: &Regex,
    limit: usize,
    include_description: bool,
    asns: &Arc<Asns>,
    cache: &mut HashMap<(IpAddr, bool), String>,
    as_open: &str,
    as_close: &str,
    as_sep: &str,
) -> String {
    let mut output = String::with_capacity(line.len());
    let mut last_end = 0;
    let mut replaced = 0;

    for caps in re_ip.captures_iter(line) {
        let whole = caps.get(0).unwrap();

        output.push_str(&line[last_end..whole.start()]);

        let replacement = if let Some(ip4) = caps.name("ip4") {
            let ip_text = ip4.as_str();

            match IpAddr::from_str(ip_text) {
                Ok(ip) if limit == 0 || replaced < limit => {
                    replaced += 1;

                    annotate_ip_token(
                        ip_text,
                        ip,
                        include_description,
                        asns,
                        cache,
                        as_open,
                        as_close,
                        as_sep,
                    )
                }
                _ => whole.as_str().to_owned(),
            }
        } else if let Some(mapped_ip4) = caps.name("mapped_ip4") {
            let ip_text = mapped_ip4.as_str();

            match IpAddr::from_str(ip_text) {
                Ok(ip) if limit == 0 || replaced < limit => {
                    replaced += 1;

                    let annotation = annotate_ip_token(
                        ip_text,
                        ip,
                        include_description,
                        asns,
                        cache,
                        as_open,
                        as_close,
                        as_sep,
                    );

                    let pre = caps
                        .name("pre_mapped")
                        .map(|m| m.as_str())
                        .unwrap_or("");

                    let mapped = caps
                        .name("mapped")
                        .map(|m| m.as_str())
                        .unwrap_or("");

                    let post = caps
                        .name("mapped_post")
                        .map(|m| m.as_str())
                        .unwrap_or("");

                    let mut result = String::with_capacity(
                        pre.len() + mapped.len() + annotation.len() + post.len(),
                    );
                    result.push_str(pre);
                    result.push_str(mapped);
                    result.push_str(&annotation);
                    result.push_str(post);
                    result
                }
                _ => whole.as_str().to_owned(),
            }
        } else if let Some(ip6) = caps.name("ip6") {
            let ip_text = ip6.as_str();

            match IpAddr::from_str(ip_text) {
                Ok(ip) if limit == 0 || replaced < limit => {
                    replaced += 1;

                    let pre = caps.name("pre").map(|m| m.as_str()).unwrap_or("");
                    let post = caps.name("post").map(|m| m.as_str()).unwrap_or("");

                    let annotation = annotate_ip_token(
                        ip_text,
                        ip,
                        include_description,
                        asns,
                        cache,
                        as_open,
                        as_close,
                        as_sep,
                    );

                    let mut result =
                        String::with_capacity(pre.len() + annotation.len() + post.len());
                    result.push_str(pre);
                    result.push_str(&annotation);
                    result.push_str(post);
                    result
                }
                _ => whole.as_str().to_owned(),
            }
        } else {
            whole.as_str().to_owned()
        };

        output.push_str(&replacement);
        last_end = whole.end();

        // Critical speed fix:
        // stop regex scanning immediately after requested replacements.
        if limit > 0 && replaced >= limit {
            output.push_str(&line[last_end..]);
            return output;
        }
    }

    output.push_str(&line[last_end..]);
    output
}

async fn annotate_mode(matches: &clap::ArgMatches) -> Result<(), i32> {
    let db_url = matches.get_one::<String>("db_url").unwrap();
    let include_description = matches.get_flag("description");
    let input_path = matches.get_one::<String>("input").map(String::as_str);
    let line_buffered = matches.get_flag("line_buffered");
    let cache_file: Option<PathBuf> =
        matches.get_one::<String>("cache_file").map(PathBuf::from);

    // Parse --first/-f limit for replacen
    // If not set, use 0. If set without value, defaults to 1. If provided with a value, use that value.
    let limit: usize = matches
        .get_one::<usize>("first")
        .copied()
        .unwrap_or(0);

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
        # IPv4-mapped IPv6. Match complete address, including delimiters.
        (?P<pre_mapped> ^ | [^0-9A-Fa-f:] )
        (?P<mapped> :: [Ff]{4} : )
        (?P<mapped_ip4> (?:\d{1,3}\.){3}\d{1,3} )
        (?P<mapped_post> [^0-9.] | $ )

        |

        # IPv4
        \b (?P<ip4> (?:\d{1,3}\.){3}\d{1,3} ) \b

        |

        # IPv6
        (?P<pre> ^ | [^0-9A-Fa-f:] )
        (?P<ip6>
            (?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4}
            | ::
        )
        (?P<post> [^0-9A-Fa-f:] | $ )
    ",
    )
    .unwrap();

    let stdout_raw = io::stdout();
    let mut stdout: Box<dyn Write> = if line_buffered && input_path.is_none() {
        Box::new(io::LineWriter::new(stdout_raw))
    } else {
        Box::new(io::BufWriter::new(stdout_raw))
    };

    let requested_threads = match matches.get_one::<usize>("threads").copied() {
        Some(0) => {
            error!("--threads must be greater than zero");
            return Err(2);
        }
        value => value,
    };

    let logical = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);

    let default_threads = (logical / 4).clamp(1, 8);
    let thread_count = requested_threads.unwrap_or(default_threads);

    // Line-buffered mode prioritizes latency and immediate output.
    // Keep processing strictly serial in this mode.
    if line_buffered {
        let mut cache: HashMap<(IpAddr, bool), String> = HashMap::new();

        for line_res in reader.lines() {
            let line = match line_res {
                Ok(line) => line,
                Err(e) => {
                    error!("Failed to read line: {}", e);
                    return Err(1);
                }
            };

            let line = replace_ip_addresses(
                &line,
                &re_ip,
                limit,
                include_description,
                &asns,
                &mut cache,
                &as_open,
                &as_close,
                as_sep,
            );

            if let Err(e) = writeln!(stdout, "{}", line) {
                error!("Failed to write output: {}", e);
                return Err(1);
            }

            // Explicit flush guarantees immediate output.
            if let Err(e) = stdout.flush() {
                error!("Failed to flush output: {}", e);
                return Err(1);
            }
        }

        return Ok(());
    }

    info!(
        "Offline annotation using {} worker threads, batch size {}, chunk size {}",
        thread_count, BATCH_SIZE, LINES_PER_CHUNK
    );

    // One-thread mode keeps one cache for the complete input.
    // Avoid batching and Rayon overhead when parallelism is disabled.
    if thread_count == 1 {
        let mut cache: HashMap<(IpAddr, bool), String> = HashMap::new();

        for line_res in reader.lines() {
            let line = match line_res {
                Ok(line) => line,
                Err(e) => {
                    error!("Failed to read line: {}", e);
                    return Err(1);
                }
            };

            let line = replace_ip_addresses(
                &line,
                &re_ip,
                limit,
                include_description,
                &asns,
                &mut cache,
                &as_open,
                &as_close,
                as_sep,
            );

            if let Err(e) = writeln!(stdout, "{}", line) {
                error!("Failed to write output: {}", e);
                return Err(1);
            }
        }

        if let Err(e) = stdout.flush() {
            error!("Failed to flush output: {}", e);
            return Err(1);
        }

        return Ok(());
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count)
        .build()
        .map_err(|e| {
            error!("Failed to create worker pool: {}", e);
            1
        })?;

    let mut lines = reader.lines();

    loop {
        let mut batch = Vec::with_capacity(BATCH_SIZE);

        for _ in 0..BATCH_SIZE {
            match lines.next() {
                Some(Ok(line)) => batch.push(line),
                Some(Err(e)) => {
                    error!("Failed to read line: {}", e);
                    return Err(1);
                }
                None => break,
            }
        }

        if batch.is_empty() {
            break;
        }

        // par_chunks keeps complete lines together.
        // Rayon collect preserves indexed input order.
        // Each chunk gets its own cache, avoiding lock contention.
        let processed_chunks: Vec<Vec<String>> = pool.install(|| {
            batch
                .par_chunks(LINES_PER_CHUNK)
                .map(|chunk| {
                    let mut cache: HashMap<(IpAddr, bool), String> = HashMap::new();

                    chunk
                        .iter()
                        .map(|line| {
                            replace_ip_addresses(
                                line,
                                &re_ip,
                                limit,
                                include_description,
                                &asns,
                                &mut cache,
                                &as_open,
                                &as_close,
                                as_sep,
                            )
                        })
                        .collect()
                })
                .collect()
        });

        for chunk in processed_chunks {
            for line in chunk {
                if let Err(e) = writeln!(stdout, "{}", line) {
                    error!("Failed to write output: {}", e);
                    return Err(1);
                }
            }
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
    ip_text: &str,
    ip: IpAddr,
    include_description: bool,
    asns: &Arc<Asns>,
    cache: &mut HashMap<(IpAddr, bool), String>,
    as_open: &str,
    as_close: &str,
    as_sep: &str,
) -> String {
    if let Some(cached) = cache.get(&(ip, include_description)) {
        return cached.clone();
    }

    let annot = if let Some(found) = asns.lookup_by_ip(ip) {
        let mut s = String::with_capacity(
            ip_text.len()
                + as_open.len()
                + as_close.len()
                + as_sep.len() * if include_description { 2 } else { 1 }
                + found.country.len()
                + found.description.len()
                + 8,
        );

        s.push_str(ip_text);
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
        let mut s = String::with_capacity(
            ip_text.len()
                + as_open.len()
                + as_close.len()
                + as_sep.len()
                + 20,
        );

        s.push_str(ip_text);
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

    cache.insert((ip, include_description), annot.clone());
    annot
}
