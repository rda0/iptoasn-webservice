use flate2::read::GzDecoder;
use hyper::body::Bytes;
use reqwest::Client;
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
    pub async fn new(url: &str) -> Result<Self, &'static str> {
        info!("Loading the database from {}", url);

        let bytes = if url.starts_with("file://") {
            // Handle local file URLs
            let path = url.strip_prefix("file://").unwrap_or(url);
            match tokio::fs::read(path).await {
                Ok(content) => Bytes::from(content),
                Err(e) => {
                    error!("Unable to read local file: {}", e);
                    return Err("Unable to read local file");
                }
            }
        } else {
            // Handle HTTP/HTTPS URLs
            let client = Client::builder()
                .user_agent("iptoasn-webservice/0.2.5")
                .build()
                .map_err(|_| {
                    error!("Failed to create HTTP client");
                    "Failed to create HTTP client"
                })?;

            let res = client.get(url).send().await.map_err(|e| {
                error!("Unable to load the database: {}", e);
                "Unable to load the database"
            })?;

            if !res.status().is_success() {
                error!("Unable to load the database, status: {}", res.status());
                return Err("Unable to load the database");
            }

            res.bytes().await.map_err(|e| {
                error!("Unable to read response body: {}", e);
                "Unable to read response body"
            })?
        };
        let mut data = String::new();
        if GzDecoder::new(bytes.as_ref())
            .read_to_string(&mut data)
            .is_err()
        {
            error!("Unable to decompress the database");
            return Err("Unable to decompress the database");
        }
        let mut asns = BTreeSet::new();
        for line in data.split_terminator('\n') {
            let mut parts = line.split('\t');
            let first_ip = IpAddr::from_str(parts.next().unwrap()).unwrap();
            let last_ip = IpAddr::from_str(parts.next().unwrap()).unwrap();
            let number = u32::from_str(parts.next().unwrap()).unwrap();
            let country = parts.next().unwrap().to_owned();
            let description = parts.next().unwrap().to_owned();
            let asn = Asn {
                first_ip,
                last_ip,
                number,
                country,
                description,
            };
            asns.insert(asn);
        }
        info!("Database loaded");
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
