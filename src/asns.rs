use flate2::read::GzDecoder;
use log::{debug, error, info, warn};
use std::cmp::{Eq, Ord, Ordering, PartialEq, PartialOrd};
use std::collections::{BTreeSet, HashMap};
use std::io::prelude::*;
use std::net::IpAddr;
use std::ops::Bound::{Included, Unbounded};
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug)]
pub struct Asn {
    pub first_ip: IpAddr,
    pub last_ip: IpAddr,
    pub number: u32,
    pub country: Arc<str>,
    pub description: Arc<str>,
}

impl PartialEq for Asn {
    fn eq(&self, other: &Self) -> bool {
        self.first_ip == other.first_ip
    }
}

impl Eq for Asn {}

impl Ord for Asn {
    fn cmp(&self, other: &Self) -> Ordering {
        self.first_ip.cmp(&other.first_ip)
    }
}

impl PartialOrd for Asn {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Asn {
    fn from_single_ip(ip: IpAddr) -> Self {
        Self {
            first_ip: ip,
            last_ip: ip,
            number: 0,
            country: Arc::from(""),
            description: Arc::from(""),
        }
    }
}

pub struct Asns {
    asns: BTreeSet<Asn>,
}

impl Asns {
    fn try_load_fallback() -> Result<Vec<u8>, &'static str> {
        let fallback_paths = [
            "cache/ip2asn-combined.tsv.gz",
            "ip2asn-combined.tsv.gz",
            "test_data.tsv.gz",
        ];

        for path in &fallback_paths {
            if let Ok(content) = std::fs::read(path) {
                info!("Successfully loaded fallback data from: {}", path);
                return Ok(content);
            } else {
                debug!("Fallback file not found: {}", path);
            }
        }

        Err("No fallback data sources available")
    }

    pub async fn new(
        url: &str,
        http_client: Option<&reqwest::Client>,
    ) -> Result<Self, &'static str> {
        info!("Loading the database from {}", url);

        let bytes = if url.starts_with("file://") {
            // Handle local file URL
            let path = url.trim_start_matches("file://");
            info!("Loading the database from file://{}", path);
            match std::fs::read(path) {
                Ok(content) => content,
                Err(e) => {
                    error!("Unable to read the database: {}", e);
                    return Err("Unable to read the database");
                }
            }
        } else if url.starts_with("http://") || url.starts_with("https://") {
            // Handle HTTP or HTTPS URL
            info!("Loading the database from {}", url);

            // Use provided client or create a new one
            let client;
            let client_ref = if let Some(provided_client) = http_client {
                provided_client
            } else {
                client = reqwest::Client::new();
                &client
            };

            // Send the request
            match client_ref
                .get(url)
                .header(
                    "User-Agent",
                    concat!("iptoasn-webservice/", env!("CARGO_PKG_VERSION")),
                )
                .send()
                .await
            {
                Ok(res) => {
                    if !res.status().is_success() {
                        error!("Unable to load the database, status: {}", res.status());
                        warn!("HTTP request failed, attempting to use cached data");

                        return match Self::try_load_fallback() {
                            Ok(content) => Self::parse_data(content),
                            Err(_) => {
                                Err("Unable to load the database and no fallback data available")
                            }
                        };
                    }

                    // Get response body as bytes
                    match res.bytes().await {
                        Ok(bytes) => bytes.to_vec(),
                        Err(e) => {
                            error!("Unable to read response body: {}", e);
                            return Err("Unable to read response body");
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to send request: {}", e);
                    warn!("Network request failed, attempting to use cached data");

                    return match Self::try_load_fallback() {
                        Ok(content) => Self::parse_data(content),
                        Err(msg) => {
                            error!("{}", msg);
                            Err("Failed to load database from URL and all fallback sources")
                        }
                    };
                }
            }
        } else {
            error!("Unsupported URL scheme: {}", url);
            return Err("Unsupported URL scheme");
        };

        // Save successful download to cache
        if url.starts_with("http://") || url.starts_with("https://") {
            Self::save_to_cache(&bytes);
        }

        Self::parse_data(bytes)
    }

    fn save_to_cache(bytes: &[u8]) {
        // Create cache directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all("cache") {
            warn!("Failed to create cache directory: {}", e);
            return;
        }

        // Save the downloaded data to cache
        match std::fs::write("cache/ip2asn-combined.tsv.gz", bytes) {
            Ok(()) => info!("Successfully cached database to cache/ip2asn-combined.tsv.gz"),
            Err(e) => warn!("Failed to cache database: {}", e),
        }
    }

    fn parse_data(bytes: Vec<u8>) -> Result<Self, &'static str> {
        let mut data = String::new();
        if GzDecoder::new(bytes.as_slice())
            .read_to_string(&mut data)
            .is_err()
        {
            error!("Unable to decompress the database");
            return Err("Unable to decompress the database");
        }

        // String interning pools to deduplicate country codes and descriptions
        let mut country_pool: HashMap<String, Arc<str>> = HashMap::new();
        let mut description_pool: HashMap<String, Arc<str>> = HashMap::new();

        let mut asns = BTreeSet::new();
        for line in data.split_terminator('\n') {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.split('\t');
            let first_ip = match parts.next().and_then(|s| IpAddr::from_str(s).ok()) {
                Some(ip) => ip,
                None => {
                    warn!("Invalid IP address in line: {}", line);
                    continue;
                }
            };
            let last_ip = match parts.next().and_then(|s| IpAddr::from_str(s).ok()) {
                Some(ip) => ip,
                None => {
                    warn!("Invalid IP address in line: {}", line);
                    continue;
                }
            };
            let number = match parts.next().and_then(|s| u32::from_str(s).ok()) {
                Some(num) => num,
                None => {
                    warn!("Invalid ASN number in line: {}", line);
                    continue;
                }
            };

            // Intern country code
            let country_str = parts.next().unwrap_or("");
            let country = country_pool
                .entry(country_str.to_owned())
                .or_insert_with(|| Arc::from(country_str))
                .clone();

            // Intern description
            let description_str = parts.next().unwrap_or("");
            let description = description_pool
                .entry(description_str.to_owned())
                .or_insert_with(|| Arc::from(description_str))
                .clone();

            let asn = Asn {
                first_ip,
                last_ip,
                number,
                country,
                description,
            };
            asns.insert(asn);
        }

        info!(
            "Database loaded with {} entries ({} unique countries, {} unique descriptions)",
            asns.len(),
            country_pool.len(),
            description_pool.len()
        );
        Ok(Self { asns })
    }

    pub fn lookup_by_ip(&self, ip: IpAddr) -> Option<&Asn> {
        let fasn = Asn::from_single_ip(ip);
        match self.asns.range((Unbounded, Included(&fasn))).next_back() {
            Some(found) if ip <= found.last_ip && found.number > 0 => Some(found),
            _ => None,
        }
    }
}
