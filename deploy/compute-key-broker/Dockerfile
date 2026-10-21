# Linux/amd64 reference resolved from the official registry on 2026-10-21.
FROM rust:1.98.1-slim-trixie@sha256:fce83543909cd5409924a61ce296bdf5bab0fca37f004c0f6c8ef6f4a37bbc59 AS builder
RUN rm -f /etc/apt/sources.list /etc/apt/sources.list.d/debian.sources && \
    printf '%s\n' 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20261020T000000Z trixie main' \
    > /etc/apt/sources.list && apt-get update && \
    apt-get install --no-install-recommends -y perl make gcc libc6-dev pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY agent-os/ agent-os/
# This file is a build input, never a secret: wrapped keys and public policy.
# Use a private temporary build context. Never commit a real tenant registry.
COPY broker-policy.json /src/broker-policy.json
WORKDIR /src/agent-os
ENV COVENANT_KEY_BROKER_POLICY=/src/broker-policy.json
RUN cargo build --locked --release -p covenant-compute-key-broker --features nitro -j2

FROM debian:trixie-slim@sha256:918311b7b6c4c6f68b232ba516584925f6c78ad82b6fd534b98979df6438e483
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
RUN rm -f /etc/apt/sources.list /etc/apt/sources.list.d/debian.sources && \
    printf '%s\n' 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20261020T000000Z trixie main' \
    > /etc/apt/sources.list && apt-get update && \
    apt-get install --no-install-recommends -y ca-certificates python3 iproute2 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/agent-os/target/release/covenant-compute-key-broker /usr/local/bin/covenant-compute-key-broker
COPY --from=builder /src/broker-policy.json /opt/covenant/broker-policy.json
COPY deploy/compute-key-broker/enclave_entrypoint.py /opt/covenant/enclave_entrypoint.py
RUN chmod 0555 /usr/local/bin/covenant-compute-key-broker /opt/covenant/enclave_entrypoint.py && chmod 0444 /opt/covenant/broker-policy.json
ENTRYPOINT ["python3", "/opt/covenant/enclave_entrypoint.py"]
