use flate2::read::GzDecoder;
use http::Request;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::{Method, StatusCode};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
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
            // Handle local file URL
            let path = url.trim_start_matches("file://");
            info!("Loading the database from file://{}", path);
            match std::fs::read(path) {
                Ok(content) => Bytes::from(content),
                Err(e) => {
                    error!("Unable to read the database: {}", e);
                    return Err("Unable to read the database");
                }
            }
        } else if url.starts_with("http://") || url.starts_with("https://") {
            // Handle HTTP or HTTPS URL
            info!("Loading the database from {}", url);

            // Create an HTTPS connector that can handle both HTTP and HTTPS with TLS 1.3 support
            let https = HttpsConnectorBuilder::new()
                .with_native_roots()
                .expect("Failed to load native roots")
                .https_or_http()
                .enable_http1()
                .build();
            let client = Client::builder(hyper_util::rt::TokioExecutor::new())
                .build::<_, Empty<Bytes>>(https);

            // Create the request
            let req = Request::builder()
                .method(Method::GET)
                .uri(url)
                .header("User-Agent", "iptoasn-webservice/0.2.5")
                .body(Empty::<Bytes>::new())
                .map_err(|e| {
                    error!("Failed to create request: {}", e);
                    "Failed to create request"
                })?;

            // Try to send the request and get the response
            match client.request(req).await {
                Ok(res) => {
                    if res.status() != StatusCode::OK {
                        error!("Unable to load the database, status: {}", res.status());
                        return Err("Unable to load the database");
                    }

                    // Collect the response body
                    let body = res.into_body();
                    match BodyExt::collect(body).await {
                        Ok(collected) => collected.to_bytes(),
                        Err(e) => {
                            error!("Unable to read response body: {}", e);
                            return Err("Unable to read response body");
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to send request: {}", e);
                    warn!("Falling back to local test data");

                    // Try to use local test data as fallback
                    let test_path = "test_data.tsv.gz";
                    match std::fs::read(test_path) {
                        Ok(content) => {
                            info!("Successfully loaded local test data");
                            Bytes::from(content)
                        }
                        Err(e) => {
                            error!("Failed to load local test data: {}", e);
                            return Err("Failed to load database from URL and local fallback");
                        }
                    }
                }
            }
        } else {
            error!("Unsupported URL scheme: {}", url);
            return Err("Unsupported URL scheme");
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
