#[macro_use]
extern crate horrorshow;
#[macro_use]
extern crate log;
#[macro_use]
extern crate router;

mod asns;
mod webservice;

use crate::asns::Asns;
use crate::webservice::WebService;
use clap::{Arg, Command};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

fn get_asns(db_url: &str) -> Result<Asns, &'static str> {
    info!("Retrieving ASNs");
    let asns = Asns::new(db_url);
    info!("ASNs loaded");
    asns
}

fn update_asns(asns_arc: &Arc<RwLock<Arc<Asns>>>, db_url: &str) {
    let asns = match get_asns(db_url) {
        Ok(asns) => asns,
        Err(e) => {
            warn!("{e}");
            return;
        }
    };
    *asns_arc.write().unwrap() = Arc::new(asns);
}

fn main() {
    let matches = Command::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .arg(
            Arg::new("listen_addr")
                .short('l')
                .long("listen")
                .value_name("ip:port")
                .help("Webservice IP and port")
                .default_value("0.0.0.0:53661"),
        )
        .arg(
            Arg::new("db_url")
                .short('u')
                .long("dburl")
                .value_name("url")
                .help("URL of the gzipped database")
                .default_value("https://iptoasn.com/data/ip2asn-combined.tsv.gz"),
        )
        .get_matches();
    let db_url = matches.get_one::<String>("db_url").unwrap().to_owned();
    let listen_addr = matches.get_one::<String>("listen_addr").unwrap().as_str();
    let asns = get_asns(&db_url).expect("Unable to load the initial database");
    let asns_arc = Arc::new(RwLock::new(Arc::new(asns)));
    let asns_arc_copy = asns_arc.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(3600));
        update_asns(&asns_arc_copy, &db_url);
    });
    info!("Starting the webservice");
    WebService::start(asns_arc, listen_addr);
}
