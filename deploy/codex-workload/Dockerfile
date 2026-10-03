# Build only through a reviewed, digest-pinned Ubuntu 26.04 base reference.
# The Core release supplies the exact checksum-bound runtime binary. No CLI
# credential, Codex auth directory, Docker socket or owner file is copied.
ARG BASE_IMAGE
FROM ${BASE_IMAGE}

ARG SOURCE_REVISION
ARG RUNTIME_SHA256
LABEL org.opencontainers.image.source="https://github.com/HawkeyNL/PersonalJarvis" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      nl.jarvis.codex-runtime.sha256="${RUNTIME_SHA256}"

RUN mkdir -p /workspace/input \
    && chown -R 65532:65532 /workspace \
    && chmod 0700 /workspace/input
COPY --chown=0:0 --chmod=0555 jarvis-codex-runtime /usr/local/bin/jarvis-codex-runtime
USER 65532:65532
WORKDIR /workspace
# OpenSandbox supplies only CODEX_SANDBOX_COMMAND through its lifecycle API.
ENTRYPOINT ["/usr/local/bin/jarvis-codex-runtime"]
