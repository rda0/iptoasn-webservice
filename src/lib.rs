#[macro_use]
extern crate horrorshow;

pub mod asns;
pub mod webservice;

// Compile-time default URL for the IP-to-ASN database.
// You can override this at build time by setting the environment variable
// IPTOASN_DB_URL, e.g.:
//   IPTOASN_DB_URL="https://example.com/ip2asn.tsv.gz" cargo build
pub const DEFAULT_DB_URL: &str = match option_env!("IPTOASN_DB_URL") {
    Some(url) => url,
    None => "https://iptoasn.com/data/ip2asn-combined.tsv.gz",
};
