# IP to ASN Web Service - Docker Guide

This guide explains how to build and run the IP to ASN web service using Docker.

## Quick Start

Build and run the service from the repository root directory:

```bash
# Build the Docker image
docker build -t iptoasn -f docker/Dockerfile .

# Run the container
docker run -itd \
           --name my-iptoasn \
           -p 8080:53661 \
           iptoasn
```

The service will start downloading the ASN database on first run. Once ready, you can query it:

```bash
curl http://localhost:8080/v1/as/ip/8.8.8.8
```

## Configuration

### Environment Variables

The service can be configured using environment variables:

| Variable | Description | Default |
|----------|-------------|---------|
| `IPTOASN_PORT` | Port to listen on | `53661` |
| `IPTOASN_DBURL` | URL to download ASN database from | Default database URL |

### Custom Configuration Example

```bash
docker run -itd \
           --name my-iptoasn \
           -e IPTOASN_PORT=10000 \
           -e IPTOASN_DBURL='https://your-database-url.com/data.tsv.gz' \
           -p 8080:10000 \
           iptoasn
```

## API Usage

Once the service is running, you can query IP addresses:

```bash
# Query a single IP
curl http://localhost:8080/v1/as/ip/8.8.8.8

# Query with JSON response
curl -H "Accept: application/json" http://localhost:8080/v1/as/ip/1.1.1.1
```

## Command Line Usage

You can also use the container as a command-line tool:

```bash
# Show help
docker run -it --rm iptoasn --help

# Run with custom parameters
docker run -it --rm iptoasn --listen 0.0.0.0:8080 --dburl https://example.com/data.tsv.gz
```

## Container Management

```bash
# Check logs
docker logs my-iptoasn

# Stop the container
docker stop my-iptoasn

# Remove the container
docker rm my-iptoasn

# Remove the image
docker rmi iptoasn
```

## Health Check

The service exposes a health endpoint that can be used for monitoring:

```bash
curl http://localhost:8080/health
```

## Troubleshooting

- **Container exits immediately**: Check logs with `docker logs my-iptoasn`
- **Service not responding**: Ensure the database download has completed
- **Port conflicts**: Change the host port mapping (e.g., `-p 9090:53661`)

## Security Notes

- The container runs as a non-root user (`app`) for security
- Only necessary packages are installed to minimize attack surface
- The final image is optimized and stripped of build dependencies
