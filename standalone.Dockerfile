# Build stage
FROM rust:1.96.0-slim-trixie AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    openssl \
    libssl-dev \
    liblua5.3-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/overload
COPY . .

# Build the application
RUN cargo build --release

# Runtime stage
FROM debian:trixie-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    openssl \
    liblua5.3-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/bin

# Copy the binary from the builder stage
COPY --from=builder /usr/src/overload/target/release/overload .

# Expose the port the app runs on
EXPOSE 3030

# Set the startup command
CMD ["/usr/bin/overload"]