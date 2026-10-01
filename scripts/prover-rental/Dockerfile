ARG PROVER_BASE_IMAGE=scratch
FROM ${PROVER_BASE_IMAGE}

# The final image is pinned by digest in the rental policy. No compile/install work
# is performed at rental startup, and only an existing standalone worker is invoked.
USER root
RUN apt-get update && apt-get install -y --no-install-recommends python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY runpod.py job.py worker.py release.json /opt/zksys-rental/
RUN python3 /opt/zksys-rental/worker.py --validate-image
ENTRYPOINT ["python3", "/opt/zksys-rental/worker.py"]
