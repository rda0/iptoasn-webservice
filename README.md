![Build Status](https://github.com/jedisct1/iptoasn-webservice/workflows/Rust/badge.svg)

# iptoasn-webservice

A high-performance HTTP API server that maps IP addresses to Autonomous System (AS) information including ASN, country code, and organization description.

This is the source code of the (previously) public API from [iptoasn.com](https://iptoasn.com).

## Features

- **Fast IP to ASN lookups** using efficient binary search over sorted IP ranges
- **Automatic database updates** with configurable refresh intervals
- **Robust data loading** with fallback mechanisms for offline operation
- **Multiple output formats** (JSON and HTML)
- **Built-in caching** for downloaded databases
- **Production-ready** with proper HTTP headers and error handling

## Requirements

- [Rust](https://www.rust-lang.org/) (latest stable)

## Installation & Usage

### Build from source

```sh
cargo build --release
```

### Run the server

```sh
# Default configuration (listen on 127.0.0.1:53661, refresh every 60 minutes)
./target/release/iptoasn-webservice

# Custom configuration
./target/release/iptoasn-webservice \
  --listen 0.0.0.0:8080 \
  --dburl https://iptoasn.com/data/ip2asn-combined.tsv.gz \
  --refresh 120
```

### Command line options

- `--listen` (`-l`): Address and port to bind to (default: `127.0.0.1:53661`)
- `--dburl` (`-u`): Database URL to download from (default: `https://iptoasn.com/data/ip2asn-combined.tsv.gz`)
- `--refresh` (`-r`): Database refresh interval in minutes, 0 to disable (default: `60`)

## API Usage

### JSON Response

```sh
curl -H'Accept: application/json' http://localhost:53661/v1/as/ip/8.8.8.8
```

```json
{
  "announced": true,
  "as_country_code": "US",
  "as_description": "GOOGLE - Google LLC",
  "as_number": 15169,
  "first_ip": "8.8.8.0",
  "ip": "8.8.8.8",
  "last_ip": "8.8.8.255"
}
```

### HTML Response

```sh
curl http://localhost:53661/v1/as/ip/8.8.8.8
```

Returns a formatted HTML page with the IP information.

### Unannounced IPs

For IP addresses not found in BGP announcements:

```json
{
  "announced": false,
  "ip": "127.0.0.1"
}
```

## Data Source

The service downloads and processes the IP-to-ASN mapping database from iptoasn.com, which provides comprehensive BGP routing table data updated regularly. The database is automatically cached locally and the service includes fallback mechanisms to continue operating even when the remote database is unavailable.
