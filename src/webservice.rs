use crate::asns::Asns;
use horrorshow::prelude::*;
use http::header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, EXPIRES, VARY};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::{SocketAddr, IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use time::macros::format_description;
use time::OffsetDateTime;
use tokio::net::TcpListener;

const TTL: u32 = 86_400;

#[derive(PartialEq, Eq)]
enum OutputType {
    Json,
    Html,
    Plain,
}

enum BodyInputType {
    Json,
    Plain,
}

#[derive(Default, Serialize, Deserialize)]
struct IpLookupResponse {
    ip: String,
    announced: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_description: Option<String>,
}

impl IpLookupResponse {
    fn not_found(ip: String) -> Self {
        Self {
            ip,
            ..Default::default()
        }
    }
}

#[derive(Serialize)]
struct AsNameResponse {
    as_number: u32,
    as_country_code: String,
    as_description: String,
}

#[derive(Serialize)]
struct AsSubnetsResponse {
    as_number: u32,
    subnets: Vec<String>,
}

pub struct WebService;

impl WebService {
    async fn handle_request(
        req: Request<hyper::body::Incoming>,
        asns_arc: Arc<RwLock<Arc<Asns>>>,
        remote_addr: SocketAddr,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let method = req.method();
        let uri = req.uri().path();

        match (method, uri) {
            (&Method::GET, "/") => Ok(Self::index()),
            (&Method::GET, "/v1/as/ip") => {
                let client_ip = Self::extract_client_ip(req.headers(), remote_addr);
                Self::ip_lookup(&client_ip, req.headers(), asns_arc)
            }
            (&Method::GET, path) if path.starts_with("/v1/as/ip/") => {
                let ip_s = path.strip_prefix("/v1/as/ip/").unwrap_or("");
                Self::ip_lookup(ip_s, req.headers(), asns_arc)
            }
            (&Method::GET, "/v1/as/n") => {
                let accept = Self::accept_type(req.headers());
                let mut resp = match accept {
                    OutputType::Plain => Response::new(Full::new(Bytes::from(
                        "Missing AS number. Use /v1/as/n/<AS123> or /v1/as/n/<123>\n",
                    ))),
                    _ => Response::new(Full::new(Bytes::from(
                        r#"{"error":"Missing AS number. Use /v1/as/n/<AS123> or /v1/as/n/<123>"}"#,
                    ))),
                };
                *resp.status_mut() = StatusCode::BAD_REQUEST;
                resp.headers_mut().insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static(match accept {
                        OutputType::Plain => "text/plain; charset=utf-8",
                        _ => "application/json; charset=utf-8",
                    }),
                );
                Ok(resp)
            }
            (&Method::GET, path) if path.starts_with("/v1/as/n/") && path.ends_with("/subnets") => {
                let asn_s = path.strip_prefix("/v1/as/n/").unwrap_or("");
                let asn_s = asn_s.strip_suffix("/subnets").unwrap_or(asn_s);
                Self::as_subnets_lookup(asn_s, req.headers(), asns_arc)
            }
            (&Method::GET, path) if path.starts_with("/v1/as/n/") => {
                let asn_s = path.strip_prefix("/v1/as/n/").unwrap_or("");
                Self::as_name_lookup(asn_s, req.headers(), asns_arc)
            }
            (&Method::PUT, "/v1/as/ips") => Self::handle_put_ips(req, asns_arc).await,
            _ => {
                let mut response = Response::new(Full::new(Bytes::from("Not Found")));
                *response.status_mut() = StatusCode::NOT_FOUND;
                Ok(response)
            }
        }
    }

    fn index() -> Response<Full<Bytes>> {
        let mut response = Response::new(Full::new(Bytes::from("iptoasn-webservice\n")));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        *response.status_mut() = StatusCode::OK;
        response
    }

    fn extract_client_ip(headers: &HeaderMap, remote_addr: SocketAddr) -> String {
        if let Some(ip_str) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            return ip_str.to_string();
        }

        if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first_ip) = forwarded
                .split(',')
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return first_ip.to_string();
            }
        }

        remote_addr.ip().to_string()
    }

    fn accept_type(headers: &HeaderMap) -> OutputType {
        if let Some(accept) = headers.get(ACCEPT) {
            if let Ok(accept_str) = accept.to_str() {
                if accept_str.contains("application/json") {
                    return OutputType::Json;
                }
                if accept_str.contains("text/plain") {
                    return OutputType::Plain;
                }
                if accept_str.contains("text/html") {
                    return OutputType::Html;
                }
            }
        }
        OutputType::Html
    }

    fn body_input_type(headers: &HeaderMap) -> Option<BodyInputType> {
        if let Some(ct) = headers.get(CONTENT_TYPE) {
            if let Ok(ct_str) = ct.to_str() {
                let ct_main = ct_str.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
                return match ct_main.as_str() {
                    "application/json" => Some(BodyInputType::Json),
                    "text/plain" => Some(BodyInputType::Plain),
                    _ => None,
                };
            }
        }
        None
    }

    fn cache_headers(headers: &mut HeaderMap) {
        let now = OffsetDateTime::now_utc();
        let expires = now + time::Duration::seconds(TTL as i64);

        let format = format_description!(
            "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
        );
        let expires_str = expires.format(&format).unwrap();

        headers.insert(
            CACHE_CONTROL,
            HeaderValue::from_str(&format!("max-age={}", TTL)).unwrap(),
        );
        headers.insert(EXPIRES, HeaderValue::from_str(&expires_str).unwrap());
        headers.insert(VARY, HeaderValue::from_static("Accept"));
    }

    fn output_json(response: &IpLookupResponse) -> Response<Full<Bytes>> {
        let json = serde_json::to_string(&response).unwrap();
        let mut response = Response::new(Full::new(Bytes::from(json)));

        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;

        response
    }

    fn output_json_vec(responses: &[IpLookupResponse]) -> Response<Full<Bytes>> {
        let json = serde_json::to_string(responses).unwrap();
        let mut response = Response::new(Full::new(Bytes::from(json)));

        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;

        response
    }

    fn output_html(response: &IpLookupResponse) -> Response<Full<Bytes>> {
        let html = html! {
            head {
                title : "iptoasn lookup";
                meta(name="viewport", content="width=device-width, initial-scale=1");
                link(rel="stylesheet", href="https://maxcdn.bootstrapcdn.com/bootstrap/4.0.0-alpha.5/css/bootstrap.min.css", integrity="sha384-AysaV+vQoT3kOAXZkl02PThvDr8HYKPZhNT5h/CXfBThSRXQ6jW5DO2ekP5ViFdi", crossorigin="anonymous");
                style : "body { margin: 1em 4em }";
            }
            body(class="container-fluid") {
                header {
                    h1 : format_args!("Information for IP address: {}", response.ip);
                }
                table {
                    tr {
                        th : "Announced";
                        td {
                            @ if response.announced {
                                : "Yes";
                            } else {
                                : "No";
                            }
                        }
                    }
                    @ if response.announced {
                        tr {
                            th : "AS Number";
                            td : format_args!("AS{}", response.as_number.unwrap());
                        }
                        tr {
                            th : "AS Range";
                            td : format_args!("{} - {}", response.first_ip.as_ref().unwrap(), response.last_ip.as_ref().unwrap());
                        }
                        tr {
                            th : "AS Country Code";
                            td : response.as_country_code.as_ref().unwrap();
                        }
                        tr {
                            th : "AS Description";
                            td : response.as_description.as_ref().unwrap();
                        }
                    }
                }
                footer {
                    p { small {
                        : "Powered by ";
                        a(href="https://iptoasn.com") : "iptoasn.com";
                    } }
                }
            }
        }.into_string()
            .unwrap();
        let html = format!("<!DOCTYPE html>\n<html>{html}</html>");

        let mut response = Response::new(Full::new(Bytes::from(html)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;

        response
    }

    fn output_plain(response: &IpLookupResponse) -> Response<Full<Bytes>> {
        let plain = if response.announced {
            format!(
                "{} | {}-{} | {} | {}",
                response.as_number.unwrap(),
                response.first_ip.as_deref().unwrap(),
                response.last_ip.as_deref().unwrap(),
                response.as_country_code.as_deref().unwrap(),
                response.as_description.as_deref().unwrap()
            )
        } else {
            format!("- | {} | - | Not announced", response.ip)
        };

        let mut response = Response::new(Full::new(Bytes::from(plain)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;

        response
    }

    fn output_plain_vec(responses: &[IpLookupResponse]) -> Response<Full<Bytes>> {
        let max_ip_len = responses.iter().map(|r| r.ip.len()).max().unwrap_or(0).max(20);
        let mut out = String::new();

        for r in responses {
            let asn_str = if r.announced {
                r.as_number.unwrap().to_string()
            } else {
                "-".to_string()
            };
            let desc_cc = if r.announced {
                format!("{}, {}", r.as_description.as_ref().unwrap(), r.as_country_code.as_ref().unwrap())
            } else {
                "Not announced".to_string()
            };
            out.push_str(&format!("{:<8} | {:<width$} | {}\n", asn_str, r.ip, desc_cc, width = max_ip_len));
        }

        let mut response = Response::new(Full::new(Bytes::from(out)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;
        response
    }

    fn output(output_type: &OutputType, response: &IpLookupResponse) -> Response<Full<Bytes>> {
        match *output_type {
            OutputType::Json => Self::output_json(response),
            OutputType::Html => Self::output_html(response),
            OutputType::Plain => Self::output_plain(response),
        }
    }

    fn ip_lookup(
        ip_s: &str,
        headers: &HeaderMap,
        asns_arc: Arc<RwLock<Arc<Asns>>>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let ip = match std::net::IpAddr::from_str(ip_s) {
            Err(_) => {
                let response = IpLookupResponse::not_found(ip_s.to_owned());
                return Ok(Self::output(&Self::accept_type(headers), &response));
            }
            Ok(ip) => ip,
        };

        let asns = asns_arc.read().unwrap().clone();

        let found = match asns.lookup_by_ip(ip) {
            None => {
                let response = IpLookupResponse::not_found(ip.to_string());
                return Ok(Self::output(&Self::accept_type(headers), &response));
            }
            Some(found) => found,
        };

        let response = IpLookupResponse {
            ip: ip.to_string(),
            announced: true,
            first_ip: Some(found.first_ip.to_string()),
            last_ip: Some(found.last_ip.to_string()),
            as_number: Some(found.number),
            as_country_code: Some(found.country.to_string()),
            as_description: Some(found.description.to_string()),
        };

        Ok(Self::output(&Self::accept_type(headers), &response))
    }

    fn parse_plain_ip_list(body: &str) -> Vec<String> {
        let mut ips = Vec::new();
        let mut in_block = false;
        let mut saw_begin = false;

        for raw_line in body.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            match line.to_ascii_lowercase().as_str() {
                "begin" => {
                    in_block = true;
                    saw_begin = true;
                    continue;
                }
                "end" => {
                    in_block = false;
                    continue;
                }
                _ => {}
            }
            if saw_begin {
                if in_block {
                    ips.push(line.to_string());
                }
            } else {
                ips.push(line.to_string());
            }
        }

        ips
    }

    async fn handle_put_ips(
        req: Request<hyper::body::Incoming>,
        asns_arc: Arc<RwLock<Arc<Asns>>>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let headers = req.headers().clone();

        let output_type = match Self::accept_type(&headers) {
            OutputType::Plain => OutputType::Plain,
            _ => OutputType::Json,
        };

        let input_type = Self::body_input_type(&headers);

        let collected = match req.into_body().collect().await {
            Ok(c) => c,
            Err(_) => {
                let mut resp = match output_type {
                    OutputType::Plain => Response::new(Full::new(Bytes::from(
                        "Failed to read request body\n",
                    ))),
                    _ => Response::new(Full::new(Bytes::from(
                        r#"{"error":"Failed to read request body"}"#,
                    ))),
                };
                *resp.status_mut() = StatusCode::BAD_REQUEST;
                resp.headers_mut().insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static(match output_type {
                        OutputType::Plain => "text/plain; charset=utf-8",
                        _ => "application/json; charset=utf-8",
                    }),
                );
                return Ok(resp);
            }
        };

        let body_bytes = collected.to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes);

        let ip_list: Vec<String> = match input_type {
            Some(BodyInputType::Json) => {
                match serde_json::from_slice::<Vec<String>>(&body_bytes) {
                    Ok(v) => v,
                    Err(_) => {
                        let looks_plain = !body_str.trim_start().starts_with('[');
                        if output_type == OutputType::Plain || looks_plain {
                            let ips = Self::parse_plain_ip_list(&body_str);
                            if ips.is_empty() {
                                let mut resp = Response::new(Full::new(Bytes::from(
                                    "Invalid text body. Expected newline-separated IPs, optionally wrapped by 'begin'/'end'\n",
                                )));
                                *resp.status_mut() = StatusCode::BAD_REQUEST;
                                resp.headers_mut().insert(
                                    CONTENT_TYPE,
                                    HeaderValue::from_static("text/plain; charset=utf-8"),
                                );
                                return Ok(resp);
                            }
                            ips
                        } else {
                            let mut resp = Response::new(Full::new(Bytes::from(
                                r#"{"error":"Invalid JSON. Expected an array of IP strings"}"#,
                            )));
                            *resp.status_mut() = StatusCode::BAD_REQUEST;
                            resp.headers_mut().insert(
                                CONTENT_TYPE,
                                HeaderValue::from_static("application/json; charset=utf-8"),
                            );
                            return Ok(resp);
                        }
                    }
                }
            }
            Some(BodyInputType::Plain) | None => {
                let ips = Self::parse_plain_ip_list(&body_str);
                if ips.is_empty() {
                    let mut resp = match output_type {
                        OutputType::Plain => Response::new(Full::new(Bytes::from(
                            "Invalid text body. Expected newline-separated IPs, optionally wrapped by 'begin'/'end'\n",
                        ))),
                        _ => Response::new(Full::new(Bytes::from(
                            r#"{"error":"Invalid text body. Expected newline-separated IPs, optionally wrapped by 'begin'/'end'"}"#,
                        ))),
                    };
                    *resp.status_mut() = StatusCode::BAD_REQUEST;
                    resp.headers_mut().insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static(match output_type {
                            OutputType::Plain => "text/plain; charset=utf-8",
                            _ => "application/json; charset=utf-8",
                        }),
                    );
                    return Ok(resp);
                }
                ips
            }
        };

        let asns = asns_arc.read().unwrap().clone();
        let mut results: Vec<IpLookupResponse> = Vec::with_capacity(ip_list.len());

        for ip_s in ip_list {
            match std::net::IpAddr::from_str(&ip_s) {
                Ok(ip) => {
                    if let Some(found) = asns.lookup_by_ip(ip) {
                        results.push(IpLookupResponse {
                            ip: ip.to_string(),
                            announced: true,
                            first_ip: Some(found.first_ip.to_string()),
                            last_ip: Some(found.last_ip.to_string()),
                            as_number: Some(found.number),
                            as_country_code: Some(found.country.to_string()),
                            as_description: Some(found.description.to_string()),
                        });
                    } else {
                        results.push(IpLookupResponse::not_found(ip_s));
                    }
                }
                Err(_) => {
                    results.push(IpLookupResponse::not_found(ip_s));
                }
            }
        }

        let mut response = match output_type {
            OutputType::Plain => Self::output_plain_vec(&results),
            _ => Self::output_json_vec(&results),
        };
        *response.status_mut() = StatusCode::OK;
        Ok(response)
    }

    fn parse_as_number(input: &str) -> Option<u32> {
        let s = input.trim();
        let s = s
            .strip_prefix("AS")
            .or_else(|| s.strip_prefix("as"))
            .unwrap_or(s);
        u32::from_str(s).ok()
    }

    fn output_as_name_json(resp: &AsNameResponse) -> Response<Full<Bytes>> {
        let json = serde_json::to_string(resp).unwrap();
        let mut response = Response::new(Full::new(Bytes::from(json)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;
        response
    }

    fn output_as_name_plain(resp: &AsNameResponse) -> Response<Full<Bytes>> {
        let plain = format!(
            "{} | {} | {}",
            resp.as_number, resp.as_country_code, resp.as_description
        );
        let mut response = Response::new(Full::new(Bytes::from(plain)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;
        response
    }

    fn output_as_name_html(resp: &AsNameResponse) -> Response<Full<Bytes>> {
        let html = html! {
            head {
                title : "iptoasn lookup";
                meta(name="viewport", content="width=device-width, initial-scale=1");
                link(rel="stylesheet", href="https://maxcdn.bootstrapcdn.com/bootstrap/4.0.0-alpha.5/css/bootstrap.min.css", integrity="sha384-AysaV+vQoT3kOAXZkl02PThvDr8HYKPZhNT5h/CXfBThSRXQ6jW5DO2ekP5ViFdi", crossorigin="anonymous");
                style : "body { margin: 1em 4em }";
            }
            body(class="container-fluid") {
                header {
                    h1 : format_args!("Information for AS number: AS{}", resp.as_number);
                }
                table {
                    tr {
                        th : "AS Number";
                        td : format_args!("AS{}", resp.as_number);
                    }
                    tr {
                        th : "AS Country Code";
                        td : &resp.as_country_code;
                    }
                    tr {
                        th : "AS Description";
                        td : &resp.as_description;
                    }
                }
                footer {
                    p { small {
                        : "Powered by ";
                        a(href="https://iptoasn.com") : "iptoasn.com";
                    } }
                }
            }
        }
        .into_string()
        .unwrap();
        let html = format!("<!DOCTYPE html>\n<html>{html}</html>");

        let mut response = Response::new(Full::new(Bytes::from(html)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;
        response
    }

    fn as_name_lookup(
        asn_s: &str,
        headers: &HeaderMap,
        asns_arc: Arc<RwLock<Arc<Asns>>>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let output_type = Self::accept_type(headers);

        let number = match Self::parse_as_number(asn_s) {
            Some(n) => n,
            None => {
                let mut resp = match output_type {
                    OutputType::Plain => Response::new(Full::new(Bytes::from(
                        "Invalid AS number. Use AS123 or 123\n",
                    ))),
                    OutputType::Html => {
                        let html = "<!DOCTYPE html><html><body><p>Invalid AS number. Use AS123 or 123</p></body></html>";
                        let mut r = Response::new(Full::new(Bytes::from(html)));
                        r.headers_mut().insert(
                            CONTENT_TYPE,
                            HeaderValue::from_static("text/html; charset=utf-8"),
                        );
                        r
                    }
                    _ => Response::new(Full::new(Bytes::from(
                        r#"{"error":"Invalid AS number. Use AS123 or 123"}"#,
                    ))),
                };
                *resp.status_mut() = StatusCode::BAD_REQUEST;
                if !resp.headers().contains_key(CONTENT_TYPE) {
                    resp.headers_mut().insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static("application/json; charset=utf-8"),
                    );
                }
                return Ok(resp);
            }
        };

        let asns = asns_arc.read().unwrap().clone();

        let resp = if let Some((country, description)) = asns.lookup_meta_by_asn(number) {
            AsNameResponse {
                as_number: number,
                as_country_code: country.to_string(),
                as_description: description.to_string(),
            }
        } else {
            AsNameResponse {
                as_number: number,
                as_country_code: "None".to_string(),
                as_description: "Not found".to_string(),
            }
        };

        let response = match output_type {
            OutputType::Plain => Self::output_as_name_plain(&resp),
            OutputType::Html => Self::output_as_name_html(&resp),
            _ => Self::output_as_name_json(&resp),
        };

        Ok(response)
    }

    fn as_subnets_lookup(
        asn_s: &str,
        headers: &HeaderMap,
        asns_arc: Arc<RwLock<Arc<Asns>>>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let output_type = Self::accept_type(headers);

        let number = match Self::parse_as_number(asn_s) {
            Some(n) => n,
            None => {
                let mut resp = match output_type {
                    OutputType::Plain => Response::new(Full::new(Bytes::from(
                        "Invalid AS number. Use AS123 or 123\n",
                    ))),
                    OutputType::Html => {
                        let html = "<!DOCTYPE html><html><body><p>Invalid AS number. Use AS123 or 123</p></body></html>";
                        let mut r = Response::new(Full::new(Bytes::from(html)));
                        r.headers_mut().insert(
                            CONTENT_TYPE,
                            HeaderValue::from_static("text/html; charset=utf-8"),
                        );
                        r
                    }
                    _ => Response::new(Full::new(Bytes::from(
                        r#"{"error":"Invalid AS number. Use AS123 or 123"}"#,
                    ))),
                };
                *resp.status_mut() = StatusCode::BAD_REQUEST;
                if !resp.headers().contains_key(CONTENT_TYPE) {
                    resp.headers_mut().insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static("application/json; charset=utf-8"),
                    );
                }
                return Ok(resp);
            }
        };

        // For AS0 (all not routed ranges) return an empty subnet list to avoid
        // trying to enumerate the complement of the routing table.
        if number == 0 {
            let subnets: Vec<String> = Vec::new();
            let response = match output_type {
                OutputType::Plain => Self::output_as_subnets_plain(&subnets),
                OutputType::Html => Self::output_as_subnets_html(number, &subnets),
                _ => {
                    let resp = AsSubnetsResponse { as_number: number, subnets };
                    Self::output_as_subnets_json(&resp)
                }
            };
            return Ok(response);
        }

        let asns = asns_arc.read().unwrap().clone();

        // If ASN is not found, return 200 with empty subnets.
        if asns.lookup_meta_by_asn(number).is_none() {
            let subnets: Vec<String> = Vec::new();
            let response = match output_type {
                OutputType::Plain => Self::output_as_subnets_plain(&subnets),
                OutputType::Html => Self::output_as_subnets_html(number, &subnets),
                _ => {
                    let resp = AsSubnetsResponse { as_number: number, subnets };
                    Self::output_as_subnets_json(&resp)
                }
            };
            return Ok(response);
        }

        // Collect ranges on-demand and deaggregate to minimal CIDR set
        let ranges = asns.collect_ranges_by_asn(number);
        let mut subnets: Vec<String> = Vec::new();
        for (first, last) in ranges {
            let first_s = first.to_string();
            let last_s = last.to_string();
            let mut parts = Self::range_to_cidrs(&first_s, &last_s);
            subnets.append(&mut parts);
        }

        let response = match output_type {
            OutputType::Plain => Self::output_as_subnets_plain(&subnets),
            OutputType::Html => Self::output_as_subnets_html(number, &subnets),
            _ => {
                let resp = AsSubnetsResponse { as_number: number, subnets };
                Self::output_as_subnets_json(&resp)
            }
        };

        Ok(response)
    }

    // Deaggregate an arbitrary inclusive range into minimal CIDR set
    fn range_to_cidrs(first_s: &str, last_s: &str) -> Vec<String> {
        let first = IpAddr::from_str(first_s).ok();
        let last = IpAddr::from_str(last_s).ok();

        match (first, last) {
            (Some(IpAddr::V4(f)), Some(IpAddr::V4(l))) => {
                let mut start = u32::from_be_bytes(f.octets());
                let end = u32::from_be_bytes(l.octets());
                if start > end {
                    return vec![];
                }
                if start == 0 && end == u32::MAX {
                    return vec!["0.0.0.0/0".to_string()];
                }
                let mut res = Vec::new();
                while start <= end {
                    let mut block: u32 = if start == 0 {
                        1u32 << 31
                    } else {
                        1u32 << start.trailing_zeros().min(31)
                    };

                    let remaining = end - start + 1;
                    while block > remaining {
                        block >>= 1;
                    }

                    let prefix_len = 32 - block.trailing_zeros() as u8;
                    let net_ip = Ipv4Addr::from(start.to_be_bytes());
                    res.push(format!("{}/{}", net_ip, prefix_len));

                    start = start.saturating_add(block);
                    if block == 0 {
                        break; // safety, shouldn't happen
                    }
                }
                res
            }
            (Some(IpAddr::V6(f)), Some(IpAddr::V6(l))) => {
                let mut start = u128::from_be_bytes(f.octets());
                let end = u128::from_be_bytes(l.octets());
                if start > end {
                    return vec![];
                }
                if start == 0 && end == u128::MAX {
                    return vec!["::/0".to_string()];
                }
                let mut res = Vec::new();
                while start <= end {
                    let mut block: u128 = if start == 0 {
                        1u128 << 127
                    } else {
                        1u128 << start.trailing_zeros().min(127)
                    };

                    let remaining = end - start + 1;
                    while block > remaining {
                        block >>= 1;
                    }

                    let prefix_len = 128 - block.trailing_zeros() as u8;
                    let net_ip = Ipv6Addr::from(start.to_be_bytes());
                    res.push(format!("{}/{}", net_ip, prefix_len));

                    start = start.saturating_add(block);
                    if block == 0 {
                        break; // safety, shouldn't happen
                    }
                }
                res
            }
            _ => vec![],
        }
    }

    fn output_as_subnets_json(resp: &AsSubnetsResponse) -> Response<Full<Bytes>> {
        let json = serde_json::to_string(resp).unwrap();
        let mut response = Response::new(Full::new(Bytes::from(json)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;
        response
    }

    fn output_as_subnets_html(as_number: u32, subnets: &[String]) -> Response<Full<Bytes>> {
        // Empty list renders as an empty <pre> content
        let body_text = if subnets.is_empty() {
            String::new()
        } else {
            subnets.join("\n")
        };

        let html = html! {
            head {
                title : "iptoasn subnets";
                meta(name="viewport", content="width=device-width, initial-scale=1");
                link(rel="stylesheet", href="https://maxcdn.bootstrapcdn.com/bootstrap/4.0.0-alpha.5/css/bootstrap.min.css", integrity="sha384-AysaV+vQoT3kOAXZkl02PThvDr8HYKPZhNT5h/CXfBThSRXQ6jW5DO2ekP5ViFdi", crossorigin="anonymous");
                style : "body { margin: 1em 4em }";
            }
            body(class="container-fluid") {
                header {
                    h1 : format_args!("Subnets for AS{}", as_number);
                }
                pre : body_text;
                footer {
                    p { small {
                        : "Powered by ";
                        a(href="https://iptoasn.com") : "iptoasn.com";
                    } }
                }
            }
        }.into_string().unwrap();
        let html = format!("<!DOCTYPE html>\n<html>{html}</html>");

        let mut response = Response::new(Full::new(Bytes::from(html)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;
        response
    }

    fn output_as_subnets_plain(subnets: &[String]) -> Response<Full<Bytes>> {
        let text = if subnets.is_empty() {
            String::new()
        } else {
            let mut s = String::new();
            for cidr in subnets {
                s.push_str(cidr);
                s.push('\n');
            }
            s
        };
        let mut response = Response::new(Full::new(Bytes::from(text)));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        Self::cache_headers(response.headers_mut());
        *response.status_mut() = StatusCode::OK;
        response
    }

    pub async fn start(asns_arc: Arc<RwLock<Arc<Asns>>>, listen_addr: &str) {
        let addr: SocketAddr = listen_addr.parse().expect("Could not parse socket address");
        let listener = match TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(e) => {
                log::error!("Failed to bind to {}: {}", addr, e);
                return;
            }
        };

        log::info!("webservice ready");

        loop {
            let (tcp, remote_addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    log::error!("Failed to accept connection: {}", e);
                    continue;
                }
            };
            let io = TokioIo::new(tcp);
            let asns_arc = asns_arc.clone();

            tokio::task::spawn(async move {
                let service = service_fn(move |req| {
                    let asns_arc = asns_arc.clone();
                    async move { Self::handle_request(req, asns_arc, remote_addr).await }
                });

                if let Err(err) = auto::Builder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await
                {
                    log::error!("Error serving connection: {:?}", err);
                }
            });
        }
    }
}
