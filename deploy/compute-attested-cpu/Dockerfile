# Official Debian linux/amd64 manifest resolved on 2026-10-20.
FROM debian:trixie-slim@sha256:918311b7b6c4c6f68b232ba516584925f6c78ad82b6fd534b98979df6438e483

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

# Immutable Debian snapshots retain signed archive metadata and exact packages.
# Disable only the time-based Valid-Until check, never signature checking.
RUN rm -f /etc/apt/sources.list /etc/apt/sources.list.d/debian.sources && \
    printf '%s\n' \
      'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20261020T000000Z trixie main' \
      'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/20261020T000000Z trixie-security main' \
      > /etc/apt/sources.list && \
    apt-get update && \
    apt-get install --no-install-recommends -y ca-certificates openssh-server python3 && \
    rm -rf /var/lib/apt/lists/* /etc/ssh/ssh_host_* && \
    groupadd --gid 10001 covenant && \
    useradd --uid 10001 --gid 10001 --home-dir /workspace --shell /bin/sh covenant && \
    usermod --password x covenant && \
    groupadd --gid 10002 collector && \
    useradd --uid 10002 --gid 10002 --no-create-home --shell /usr/sbin/nologin collector && \
    mkdir -p /opt/covenant /state /host-public /workspace && \
    chown 10002:10002 /state && chmod 700 /state && \
    chown 10001:10001 /workspace && chmod 700 /workspace && \
    dpkg-query -W -f='${binary:Package}\t${Version}\n' > /opt/covenant/packages.tsv

COPY deploy/compute-attested-cpu/collector.py \
     deploy/compute-attested-cpu/memory_guard.py \
     deploy/compute-attested-cpu/ephemeral_collector.py \
     deploy/compute-attested-cpu/outbound_worker.py \
     deploy/compute-attested-cpu/relay_client.py \
     deploy/compute-attested-cpu/transport_protocol.py \
     deploy/compute-attested-cpu/entrypoint.py /opt/covenant/
RUN chmod 0555 /opt/covenant/*.py && chmod 0444 /opt/covenant/packages.tsv && \
    python3 -m py_compile /opt/covenant/collector.py /opt/covenant/entrypoint.py \
      /opt/covenant/memory_guard.py /opt/covenant/ephemeral_collector.py \
      /opt/covenant/outbound_worker.py /opt/covenant/transport_protocol.py \
      /opt/covenant/relay_client.py

WORKDIR /opt/covenant
EXPOSE 1022 8090
ENTRYPOINT ["python3", "/opt/covenant/entrypoint.py"]
