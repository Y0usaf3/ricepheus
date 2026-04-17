# --- Build Stage ---
FROM rust:1.80-slim-bookworm AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the source code and templates
# Note: .env is required at compile time because of the dotenv! macro
COPY .env .env
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates

# Build the application
RUN cargo build --release

# --- Runtime Stage ---
FROM debian:bookworm-slim

# Install runtime dependencies (ca-certificates for HTTPS requests)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from the builder stage
COPY --from=builder /app/target/release/ricepheus /app/ricepheus

# Copy templates as they are loaded at runtime
COPY --from=builder /app/templates /app/templates

# Expose the application port
EXPOSE 5555

# Run the application
CMD ["/app/ricepheus"]
