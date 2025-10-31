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

To change the compile time defaults for the DB or server URL:

```sh
IPTOASN_DB_URL="https://example.com/data/ip2asn-combined.tsv.gz" \  # default URL to download DB (cli, webservice)
IPTOASN_SERVER_URL="https://example.com" \                          # default URL to query the API (cli)
cargo build --release
```

### Run the server

Example using default configuration (listen on `127.0.0.1:53661`, refresh every `60` minutes):

```sh
./target/release/iptoasn-webservice
```

Example using custom configuration:

```sh
./target/release/iptoasn-webservice \
  --listen 0.0.0.0:8080 \
  --dburl https://iptoasn.com/data/ip2asn-combined.tsv.gz \
  --refresh 120
```

Usage:

```sh
target/release/iptoasn-webservice -h
IP to ASN webservice

Usage: iptoasn-webservice [OPTIONS]

Options:
  -l, --listen <listen_addr>     Address:port to listen to [default: 127.0.0.1:53661]
  -c, --cache-file <path>        Path to cache file [default: cache/ip2asn-combined.tsv.gz]
  -u, --dburl <db_url>           URL of the database [env: IPTOASN_DB_URL=] [default:
                                 https://iptoasn.com/data/ip2asn-combined.tsv.gz]
  -r, --refresh <refresh_delay>  Database refresh delay (minutes, 0 to disable) [default: 60]
  -h, --help                     Print help
  -V, --version                  Print version
```

### Use the CLI tool

The CLI tool can be used to annotate IP addresses in log files (i.e. webserver logs) or output of other CLI tools
(i.e. `ss`) with AS info (AS number, AS country code and optional AS description).

Examples:

```sh
tail -f /var/log/apache2/access.log | iptoasn -dl
iptoasn -di /var/log/apache2/access.log

8.8.8.8 [AS15169, US, GOOGLE] - - [27/Oct/2025:12:10:13 +0100] "GET /dns/root.hints HTTP/1.1" 500 3510 839 2729 "-" "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)" TLSv1.3 TLS_AES_128_GCM_SHA256 Initial
```

Subcommands can be used to query the webservice.

Examples:

```sh
$ iptoasn ip 8.8.8.8
15169 | 8.8.8.0-8.8.8.255 | US | GOOGLE
$ iptoasn asn 15169
15169 | US | GOOGLE
$ echo -e '8.8.8.8\n8.8.4.4' | iptoasn ips
15169    | 8.8.8.8              | GOOGLE, US
15169    | 8.8.4.4              | GOOGLE, US
$ echo '["8.8.8.8","8.8.4.4"]' | iptoasn ips
15169    | 8.8.8.8              | GOOGLE, US
15169    | 8.8.4.4              | GOOGLE, US
$ echo -e '8.8.8.8\n8.8.4.4' > ip_list.txt
$ iptoasn ips ip_list.txt
15169    | 8.8.8.8              | GOOGLE, US
15169    | 8.8.4.4              | GOOGLE, US
$ echo '["8.8.8.8","8.8.4.4"]' > ip_list.json
$ iptoasn ips ip_list.json
15169    | 8.8.8.8              | GOOGLE, US
15169    | 8.8.4.4              | GOOGLE, US
$ iptoasn asn subnets 15169 | head -n2
8.8.4.0/24
8.8.8.0/24
$ iptoasn asns | rg -S google | head -n2
15169 | US | GOOGLE
16550 | US | GOOGLE-PRIVATE-CLOUD
$ iptoasn --json ip 8.8.8.8  # all subcommands support JSON output
{"ip":"8.8.8.8","announced":true,"first_ip":"8.8.8.0","last_ip":"8.8.8.255","as_number":15169,"as_country_code":"US","as_description":"GOOGLE"}
```

Usage:

```sh
cp target/release/iptoasn /usr/local/bin
iptoasn -h
Annotate IP addresses with ASN info using in-memory database. Subcommands query the iptoasn webservice

Usage: iptoasn [OPTIONS] [COMMAND]

Commands:
  ip    Lookup IP via webservice
  ips   Bulk IP lookup via webservice; reads IPs from file or stdin. Input can be text/plain or JSON (auto-detected).
  asn   AS number lookup via webservice, or subcommands
  asns  List all AS numbers via webservice
  help  Print this message or the help of the given subcommand(s)

Options:
      --server <url>       Base URL of iptoasn webservice [env: IPTOASN_SERVER_URL=] [default:
                           http://127.0.0.1:53661]
  -j, --json               Use JSON format for output of subcommands (Accept: application/json)
  -u, --dburl <db_url>     URL to download the in-memory database [env: IPTOASN_DB_URL=] [default:
                           https://iptoasn.com/data/ip2asn-combined.tsv.gz]
  -c, --cache-file <path>  Override path to cache file [env: $XDG_CACHE_HOME/iptoasn/] [default: ~/.cache/iptoasn/]
  -i, --input <path>       Path to input file (defaults to stdin)
  -d, --description        Include AS description in annotations
  -l, --line-buffered      Flush each output line immediately when reading from stdin
  -m, --as-markers <pair>  Two characters: opening and closing marker for AS info (e.g., [] or <>) [default: []]
  -s, --as-sep <str>       Delimiter between AS info fields [default: ", "]
  -h, --help               Print help
  -V, --version            Print version
```

## API Usage

### Routes

- `GET /v1/as/ip/<ip address>`
  - Lookup provided IP address
- `GET /v1/as/ip`
  - Lookup requester's IP address, prioritized as X-Real-IP > X-Forwarded-For > Request IP
- `PUT /v1/as/ips`
  - Bulk lookup provided list of IP addresses
- `GET /v1/as/n/<as number>`
  - Lookup provided AS number
- `GET /v1/as/ns`
  - Returns all known AS numbers
- `GET /v1/as/n/<as number>/subnets`
  - Returns all known subnets of a given AS number

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
15169 | 8.8.8.0-8.8.8.255 | US | GOOGLE
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

### AS Numbers lookup

This endpoint returns all known AS numbers:

```sh
curl -sH'Accept: text/plain' http://localhost:53661/v1/as/ns | rg -S google
xh http://localhost:53661/v1/as/ns Accept:text/plain | rg -S google
15169 | US | GOOGLE
16550 | US | GOOGLE-PRIVATE-CLOUD
16591 | US | GOOGLE-FIBER
19527 | US | GOOGLE-2
...
```

### AS Subnets lookup

This endpoint returns all IP subnets of a given AS in CIDR format:

```sh
curl -H'Accept: application/json' http://localhost:53661/v1/as/n/15169/subnets
xh http://localhost:53661/v1/as/n/AS15169/subnets Accept:application/json

{
    "as_number": 15169,
    "subnets": [
        "8.8.4.0/24",
        "8.8.8.0/24",
        ...
    ]
}
```

Or as plaintext:

```sh
curl -H'Accept: text/plain' http://localhost:53661/v1/as/n/15169/subnets
xh http://localhost:53661/v1/as/n/15169/subnets Accept:text/plain

8.8.4.0/24
8.8.8.0/24
...
```

Note: These subnets are not necessarily exactly the same as the announced prefixes in BGP,
because the subnets may contain multiple adjacent announced prefixes of the same AS.

The in BGP announced prefixes can be queried from the ripe database:
https://stat.ripe.net/docs/data-api/api-endpoints/announced-prefixes

## Data Source

The service downloads and processes the IP-to-ASN mapping database from iptoasn.com, which provides comprehensive BGP routing table data updated regularly. The database is automatically cached locally and the service includes fallback mechanisms to continue operating even when the remote database is unavailable.
