use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use iptoasn_webservice::asns::Asns;
use iptoasn_webservice::webservice::WebService;
use clap::{Arg, Command};
use log::{error, info, warn};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    env_logger::init();

    let matches = Command::new("iptoasn-webservice")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Frank Denis <github@pureftpd.org>")
        .about("IP to ASN webservice")
        .arg(
            Arg::new("listen_addr")
                .short('l')
                .long("listen")
                .value_name("listen_addr")
                .help("Address:port to listen to")
                .default_value("127.0.0.1:53661"),
        )
        .arg(
            Arg::new("cache_file")
                .short('c')
                .long("cache-file")
                .value_name("path")
                .help("Path to cache file")
                .default_value("cache/ip2asn-combined.tsv.gz"),
        )
        .arg(
            Arg::new("db_url")
                .short('u')
                .long("dburl")
                .value_name("db_url")
                .help("URL of the database")
                .default_value("https://iptoasn.com/data/ip2asn-combined.tsv.gz"),
        )
        .arg(
            Arg::new("refresh_delay")
                .short('r')
                .long("refresh")
                .value_name("refresh_delay")
                .help("Database refresh delay (minutes, 0 to disable)")
                .default_value("60")
                .value_parser(clap::value_parser!(u64)),
        )
        .get_matches();

    let db_url = matches.get_one::<String>("db_url").unwrap();
    let listen_addr = matches.get_one::<String>("listen_addr").unwrap();
    let refresh_delay = *matches.get_one::<u64>("refresh_delay").unwrap();
    let cache_file: PathBuf = PathBuf::from(matches.get_one::<String>("cache_file").unwrap());

    // Create HTTP client once if URL is HTTP/HTTPS
    let http_client = if db_url.starts_with("http://") || db_url.starts_with("https://") {
        Some(reqwest::Client::new())
    } else {
        None
    };

    let asns = match get_asns(db_url, http_client.as_ref(), Some(cache_file.clone())).await {
        Ok(asns) => asns,
        Err(e) => {
            error!("Failed to load initial database: {e}");
            error!("Application cannot start without initial data");
            return;
        }
    };
    let asns_arc = Arc::new(RwLock::new(Arc::new(asns)));

    // Only start the refresh task if refresh_delay > 0
    if refresh_delay > 0 {
        let asns_arc_t = asns_arc.clone();
        let db_url_t = db_url.clone();
        let http_client_t = http_client.clone();
        let cache_file_t = cache_file.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(refresh_delay * 60)).await;
                update_asns(
                    &asns_arc_t,
                    &db_url_t,
                    http_client_t.as_ref(),
                    Some(cache_file_t.clone()),
                )
                .await;
            }
        });
        info!(
            "Automatic database refresh enabled (every {} minutes)",
            refresh_delay
        );
    } else {
        info!("Automatic database refresh disabled");
    }

    WebService::start(asns_arc, listen_addr).await;
}

async fn get_asns(
    db_url: &str,
    http_client: Option<&reqwest::Client>,
    cache_file: Option<PathBuf>,
) -> Result<Asns, &'static str> {
    info!("Retrieving ASNs");
    let asns = Asns::new(db_url, http_client, cache_file).await?;
    info!("ASNs loaded");
    Ok(asns)
}

async fn update_asns(
    asns_arc: &Arc<RwLock<Arc<Asns>>>,
    db_url: &str,
    http_client: Option<&reqwest::Client>,
    cache_file: Option<PathBuf>,
) {
    info!("Attempting to update ASN database");
    let asns = match get_asns(db_url, http_client, cache_file).await {
        Ok(asns) => asns,
        Err(e) => {
            warn!("Failed to update ASN database: {e}");
            warn!("Continuing with existing data");
            return;
        }
    };
    let asns_arc_new = Arc::new(asns);
    let mut asns_arc_w = asns_arc.write().unwrap();
    *asns_arc_w = asns_arc_new;
    info!("ASN database successfully updated");
}
