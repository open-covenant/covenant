# New candidate only. Extending this source-qualified guest does not qualify
# the new recipient image, Compose measurement, or broker key-release policy.
FROM ghcr.io/open-covenant/covenant/compute-node-public@sha256:925122b9c68fe8f385784b3d926685f6e08f59467c5a76fbfe71faa701d81932

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

# The parent retains immutable signed Debian 20261020 snapshot sources. Never
# switch to rolling package mirrors or disable archive signature verification.
RUN apt-get update && \
    apt-get install --no-install-recommends -y python3-venv && \
    rm -rf /var/lib/apt/lists/* && \
    python3 -m venv /opt/covenant-key-recipient-venv
COPY deploy/compute-key-broker/guest/requirements.txt /opt/covenant-key-broker/guest/requirements.txt
RUN /opt/covenant-key-recipient-venv/bin/pip install --no-cache-dir \
      --only-binary=:all: --require-hashes \
      -r /opt/covenant-key-broker/guest/requirements.txt && \
    /opt/covenant-key-recipient-venv/bin/pip check
COPY deploy/compute-key-broker/guest/recipient.py /opt/covenant-key-broker/guest/recipient.py
RUN chmod 0555 /opt/covenant-key-broker/guest/recipient.py && \
    chmod 0444 /opt/covenant-key-broker/guest/requirements.txt && \
    /opt/covenant-key-recipient-venv/bin/python -m py_compile /opt/covenant-key-broker/guest/recipient.py && \
    dpkg-query -W -f='${binary:Package}\t${Version}\n' > /opt/covenant-key-broker/guest/packages.tsv && \
    chmod 0444 /opt/covenant-key-broker/guest/packages.tsv

WORKDIR /opt/covenant-key-broker/guest
USER 10001:10001
ENTRYPOINT ["/opt/covenant-key-recipient-venv/bin/python", "/opt/covenant-key-broker/guest/recipient.py"]
