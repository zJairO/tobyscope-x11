FROM rust:1-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libx11-dev \
        libxcomposite-dev \
        libxrender-dev \
        libxdamage-dev \
        libxfixes-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work
CMD ["cargo", "check"]
