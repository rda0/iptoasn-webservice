use flate2::read::GzDecoder;
use std::cmp::{Eq, Ord, Ordering, PartialEq, PartialOrd};
use std::collections::BTreeSet;
use std::io::prelude::*;
use std::net::IpAddr;
use std::ops::Bound::{Included, Unbounded};
use std::str::FromStr;

#[derive(Debug)]
pub struct Asn {
    pub first_ip: IpAddr,
    pub last_ip: IpAddr,
    pub number: u32,
    pub country: String,
    pub description: String,
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
    const fn from_single_ip(ip: IpAddr) -> Self {
        Self {
            first_ip: ip,
            last_ip: ip,
            number: 0,
            country: String::new(),
            description: String::new(),
        }
    }
}

pub struct Asns {
    asns: BTreeSet<Asn>,
}

impl Asns {
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
                .header("User-Agent", "iptoasn-webservice/0.2.5")
                .send()
                .await
            {
                Ok(res) => {
                    if !res.status().is_success() {
                        error!("Unable to load the database, status: {}", res.status());
                        warn!("HTTP request failed, attempting to use cached data");

                        // Try fallback sources on HTTP errors too
                        let fallback_paths = [
                            "cache/ip2asn-combined.tsv.gz",
                            "ip2asn-combined.tsv.gz",
                            "test_data.tsv.gz",
                        ];

                        for path in &fallback_paths {
                            match std::fs::read(path) {
                                Ok(content) => {
                                    info!("Successfully loaded fallback data from: {}", path);
                                    return Self::parse_data(content);
                                }
                                Err(_) => {
                                    debug!("Fallback file not found: {}", path);
                                }
                            }
                        }

                        return Err("Unable to load the database and no fallback data available");
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

                    // Try multiple fallback sources in order of preference
                    let fallback_paths = [
                        "cache/ip2asn-combined.tsv.gz", // Primary cache location
                        "ip2asn-combined.tsv.gz",       // Local copy
                        "test_data.tsv.gz",             // Test data fallback
                    ];

                    for path in &fallback_paths {
                        match std::fs::read(path) {
                            Ok(content) => {
                                info!("Successfully loaded fallback data from: {}", path);
                                return Self::parse_data(content);
                            }
                            Err(_) => {
                                debug!("Fallback file not found: {}", path);
                            }
                        }
                    }

                    error!("No fallback data sources available");
                    return Err("Failed to load database from URL and all fallback sources");
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
            let country = parts.next().unwrap_or("").to_owned();
            let description = parts.next().unwrap_or("").to_owned();
            let asn = Asn {
                first_ip,
                last_ip,
                number,
                country,
                description,
            };
            asns.insert(asn);
        }
        info!("Database loaded with {} entries", asns.len());
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
