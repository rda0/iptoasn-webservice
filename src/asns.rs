use flate2::read::GzDecoder;
use log::{debug, error, info, warn};
use std::cmp::{Eq, Ord, Ordering, PartialEq, PartialOrd};
use std::collections::{BTreeSet, HashMap};
use std::io::prelude::*;
use std::net::IpAddr;
use std::ops::Bound::{Included, Unbounded};
use std::str::FromStr;
use std::sync::Arc;
use std::{env, fs};
use std::path::{Path, PathBuf};

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
    asn_meta: HashMap<u32, (Arc<str>, Arc<str>)>,
}

impl Asns {
    const CACHE_FILE_NAME: &'static str = "ip2asn-combined.tsv.gz";
    const CACHE_SUBDIR: &'static str = "iptoasn";

    fn default_cache_file_path() -> Option<PathBuf> {
        if let Ok(xdg_cache) = env::var("XDG_CACHE_HOME") {
            return Some(PathBuf::from(xdg_cache)
                .join(Self::CACHE_SUBDIR)
                .join(Self::CACHE_FILE_NAME));
        }
        if let Some(home_dir) = home::home_dir() {
            return Some(home_dir
                .join(".cache")
                .join(Self::CACHE_SUBDIR)
                .join(Self::CACHE_FILE_NAME));
        }
        None
    }

    fn try_load_fallback(cache_file: Option<&Path>) -> Result<Vec<u8>, &'static str> {
        // 1) CLI-provided cache path
        if let Some(cf) = cache_file {
            match fs::read(cf) {
                Ok(content) => {
                    info!("Successfully loaded fallback data from: {}", cf.display());
                    return Ok(content);
                }
                Err(_) => {
                    debug!("Fallback file not found: {}", cf.display());
                }
            }
        }

        // 2) Default XDG-based cache path
        if let Some(def) = Self::default_cache_file_path() {
            match fs::read(&def) {
                Ok(content) => {
                    info!("Successfully loaded fallback data from: {}", def.display());
                    return Ok(content);
                }
                Err(_) => {
                    debug!("Fallback file not found: {}", def.display());
                }
            }
        }

        // 3) Legacy/local development fallback paths for backward compatibility
        let fallback_paths = [
            "cache/ip2asn-combined.tsv.gz",
            "ip2asn-combined.tsv.gz",
            "test_data.tsv.gz",
        ];

        for path in &fallback_paths {
            if let Ok(content) = fs::read(path) {
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
        cache_file: Option<PathBuf>,
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

                        return match Self::try_load_fallback(cache_file.as_deref()) {
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

                    return match Self::try_load_fallback(cache_file.as_deref()) {
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
            Self::save_to_cache(&bytes, cache_file.as_deref());
        }

        Self::parse_data(bytes)
    }

    fn save_to_cache(bytes: &[u8], cache_file: Option<&Path>) {
        let target_path = cache_file
            .map(|p| p.to_path_buf())
            .or_else(Self::default_cache_file_path);
        let Some(path) = target_path else {
            warn!("No cache path available; skipping cache save");
            return;
        };

        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                warn!("Failed to create cache directory {}: {}", parent.display(), e);
                return;
            }
        }

        match fs::write(&path, bytes) {
            Ok(()) => info!("Successfully cached database to {}", path.display()),
            Err(e) => warn!("Failed to cache database to {}: {}", path.display(), e),
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
        let mut asn_meta: HashMap<u32, (Arc<str>, Arc<str>)> = HashMap::new();

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
                country: country.clone(),
                description: description.clone(),
            };
            asns.insert(asn);

            // Store AS meta (country + description) if not already present
            asn_meta.entry(number).or_insert_with(|| (country, description));
        }

        info!(
            "Database loaded with {} entries ({} unique countries, {} unique descriptions)",
            asns.len(),
            country_pool.len(),
            description_pool.len()
        );
        Ok(Self { asns, asn_meta })
    }

    pub fn lookup_by_ip(&self, ip: IpAddr) -> Option<&Asn> {
        let fasn = Asn::from_single_ip(ip);
        match self.asns.range((Unbounded, Included(&fasn))).next_back() {
            Some(found) if ip <= found.last_ip && found.number > 0 => Some(found),
            _ => None,
        }
    }

    pub fn lookup_meta_by_asn(&self, number: u32) -> Option<(Arc<str>, Arc<str>)> {
        self.asn_meta
            .get(&number)
            .map(|(cc, desc)| (cc.clone(), desc.clone()))
    }

    // Build a temporary list of ranges for a given ASN by scanning the in-memory set.
    // No persistent memory overhead; O(N) per call.
    pub fn collect_ranges_by_asn(&self, number: u32) -> Vec<(IpAddr, IpAddr)> {
        self.asns
            .iter()
            .filter(|a| a.number == number)
            .map(|a| (a.first_ip, a.last_ip))
            .collect()
    }
}
