#[macro_use]
extern crate horrorshow;
#[macro_use]
extern crate log;

mod asns;
mod webservice;

use crate::asns::Asns;
use crate::webservice::WebService;
use clap::{Arg, Command};
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[tokio::main]
async fn main() {
    env_logger::init();

    let matches = Command::new("iptoasn-webservice")
        .version("0.2.5")
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
                .default_value("60"),
        )
        .get_matches();

    let db_url = matches.get_one::<String>("db_url").unwrap();
    let listen_addr = matches.get_one::<String>("listen_addr").unwrap();
    let refresh_delay = matches.get_one::<String>("refresh_delay").unwrap();
    let refresh_delay = refresh_delay.parse::<u64>().unwrap();

    let asns = match get_asns(db_url).await {
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
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(refresh_delay * 60)).await;
                update_asns(&asns_arc_t, &db_url_t).await;
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

async fn get_asns(db_url: &str) -> Result<Asns, &'static str> {
    info!("Retrieving ASNs");
    let asns = Asns::new(db_url).await?;
    info!("ASNs loaded");
    Ok(asns)
}

async fn update_asns(asns_arc: &Arc<RwLock<Arc<Asns>>>, db_url: &str) {
    info!("Attempting to update ASN database");
    let asns = match get_asns(db_url).await {
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
