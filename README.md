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

### Routes

- `/v1/as/ip/<ip address>`
  - Lookup provided IP address
- `/v1/as/ip`
  - Lookup requester's IP address, prioritized as X-Real-IP > X-Forwarded-For > Request IP
- `/v1/as/ips`
  - Bulk lookup provided list of IP addresses
- `/v1/as/n/<as number>`
  - Lookup provided AS number

### JSON Response

```sh
curl -H'Accept: application/json' http://localhost:53661/v1/as/ip/8.8.8.8
xh http://localhost:53661/v1/as/ip/8.8.8.8 Accept:application/json
```

Returns json:

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
xh http://localhost:53661/v1/as/ip/8.8.8.8
```

Returns a formatted HTML page with the IP information.

### Plain Response

```sh
curl -H'Accept: text/plain' http://localhost:53661/v1/as/ip/8.8.8.8
xh http://localhost:53661/v1/as/ip/8.8.8.8 Accept:text/plain
```

Returns a plaintext response in the format:

```
15169 | 8.8.8.0-8.8.8.255 | GOOGLE, US
```

### Bulk IP JSON Response

```sh
echo '["8.8.8.8","8.8.4.4"]' | curl -H "Accept: application/json" -X PUT --json @- http://localhost:53661/v1/as/ips
echo '["8.8.8.8","8.8.4.4"]' | xh PUT http://localhost:53661/v1/as/ips Accept:application/json
```

Returns json:

```json
[
  {
    "ip": "8.8.8.8",
    "announced": true,
    "first_ip": "8.8.8.0",
    "last_ip": "8.8.8.255",
    "as_number": 15169,
    "as_country_code": "US",
    "as_description": "GOOGLE"
  },
  {
    "ip": "8.8.4.4",
    "announced": true,
    "first_ip": "8.8.4.0",
    "last_ip": "8.8.4.255",
    "as_number": 15169,
    "as_country_code": "US",
    "as_description": "GOOGLE"
  }
]
```

### Bulk IP Plain Response

```sh
echo -e '8.8.8.8\n8.8.4.4' | curl -H "Accept: text/plain" -X PUT --data-binary @- http://localhost:53661/v1/as/ips
echo -e '8.8.8.8\n8.8.4.4' | xh PUT http://localhost:53661/v1/as/ips Accept:text/plain
```

or alternatively:

```sh
echo -e 'begin\n8.8.8.8\n8.8.4.4\nend' | xh PUT http://localhost:53661/v1/as/ips Accept:text/plain
```

Returns a plaintext response in the format:

```
15169    | 8.8.8.8              | GOOGLE, US
15169    | 8.8.4.4              | GOOGLE, US
```

### Unannounced IPs

For IP addresses not found in BGP announcements:

```json
{
  "announced": false,
  "ip": "127.0.0.1"
}
```

### AS Number lookup

ASNs can be provided in format `15169` or `AS15169`:

```sh
curl -H'Accept: application/json' http://localhost:53661/v1/as/n/15169
xh http://localhost:53661/v1/as/n/AS15169 Accept:application/json
```

Or for a plaintext response:

```sh
curl -H'Accept: text/plain' http://localhost:53661/v1/as/n/15169
xh http://localhost:53661/v1/as/n/15169 Accept:text/plain
```

Response format:

```
15169 | US | GOOGLE
```

## Data Source

The service downloads and processes the IP-to-ASN mapping database from iptoasn.com, which provides comprehensive BGP routing table data updated regularly. The database is automatically cached locally and the service includes fallback mechanisms to continue operating even when the remote database is unavailable.
