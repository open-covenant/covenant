# Covenant Kani verifier image. BASE_IMAGE is mandatory and must be digest-pinned
# (repo@sha256:...). Authoring note: this file has NOT been built by its author.
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ARG BASE_IMAGE

ARG KANI_VERSION=0.68.0
ARG RUST_TOOLCHAIN=nightly-2026-08-21

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    KANI_HOME=/opt/kani \
    PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin \
    HOME=/tmp \
    CARGO_NET_OFFLINE=true \
    CARGO_TERM_COLOR=never

# Network is only used at build time. Fail the build if the base is not digest-pinned.
RUN case "${BASE_IMAGE}" in *@sha256:????????????????????????????????????????????????????????????????) ;; \
      *) echo "BASE_IMAGE must be digest pinned" >&2; exit 1;; esac \
 && apt-get update \
 && apt-get install -y --no-install-recommends python3 ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && rustup toolchain install "${RUST_TOOLCHAIN}" --profile minimal \
 && rustup default "${RUST_TOOLCHAIN}" \
 && CARGO_NET_OFFLINE=false cargo install --locked kani-verifier --version "${KANI_VERSION}" \
 && CARGO_NET_OFFLINE=false cargo-kani setup \
 && chmod -R a+rX /opt/kani /usr/local/cargo /usr/local/rustup

COPY runner.py /opt/covenant/kani/runner.py

RUN chmod 0444 /opt/covenant/kani/runner.py \
 && mkdir -p /opt/covenant/kani /work /evidence /scratch \
 && chmod 0777 /work /evidence /scratch \
 && { echo "kani=$(cargo-kani --version)"; \
      echo "cbmc=$(/opt/kani/kani-${KANI_VERSION}/bin/cbmc --version 2>&1 | head -n1)"; \
      echo "rustc=$(rustc --version)"; \
      echo "python=$(python3 --version)"; \
      echo "base_image=${BASE_IMAGE}"; } > /opt/covenant/kani/versions.txt \
 && chmod 0444 /opt/covenant/kani/versions.txt

WORKDIR /work
# Any numeric UID may run this image; rootfs may be read-only; no network needed.
ENTRYPOINT ["python3", "/opt/covenant/kani/runner.py"]
CMD ["run", "--plan", "/request/plan.json", "--workdir", "/work", "--outdir", "/evidence"]
