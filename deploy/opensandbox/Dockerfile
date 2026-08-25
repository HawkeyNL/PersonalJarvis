# Build from the exact reviewed upstream commit and apply the minimal Home Node
# loopback-port, egress and sidecar-hardening patches. The operator verifies
# the resulting image before use.
ARG UV_IMAGE
ARG PYTHON_BASE_IMAGE
FROM ${UV_IMAGE} AS uv

FROM ${PYTHON_BASE_IMAGE} AS builder
ARG OPENSANDBOX_COMMIT
RUN test -n "$OPENSANDBOX_COMMIT" \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git patch \
    && rm -rf /var/lib/apt/lists/* \
    && git clone https://github.com/opensandbox-group/OpenSandbox.git /src \
    && git -C /src checkout --detach "$OPENSANDBOX_COMMIT" \
    && test "$(git -C /src rev-parse HEAD)" = "$OPENSANDBOX_COMMIT"

COPY patches/0001-loopback-published-ports.patch \
     patches/0002-egress-private-range-deny.patch \
     patches/0003-egress-sidecar-hardening.patch \
     patches/0004-egress-sidecar-resource-limits.patch /tmp/
RUN git -C /src apply --check /tmp/0001-loopback-published-ports.patch \
    && git -C /src apply --check /tmp/0002-egress-private-range-deny.patch \
    && git -C /src apply --check /tmp/0003-egress-sidecar-hardening.patch \
    && git -C /src apply --check /tmp/0004-egress-sidecar-resource-limits.patch \
    && git -C /src apply /tmp/0001-loopback-published-ports.patch \
    && git -C /src apply /tmp/0002-egress-private-range-deny.patch \
    && git -C /src apply /tmp/0003-egress-sidecar-hardening.patch \
    && git -C /src apply /tmp/0004-egress-sidecar-resource-limits.patch

WORKDIR /app
RUN cp /src/server/pyproject.toml /src/server/uv.lock /src/server/LICENSE /src/server/README.md . \
    && cp -R /src/server/opensandbox_server .
COPY --from=uv /uv /uvx /bin/
ENV UV_PROJECT_ENV=/app/.venv UV_LINK_MODE=copy
RUN uv sync --frozen --no-dev --no-install-project \
    && uv pip install --no-deps --editable .

FROM ${PYTHON_BASE_IMAGE}
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 \
    PATH="/app/.venv/bin:${PATH}" \
    SANDBOX_CONFIG_PATH=/etc/opensandbox/opensandbox.toml
COPY --from=builder /app/.venv /app/.venv
COPY --from=builder /app/opensandbox_server /app/opensandbox_server
ENTRYPOINT ["opensandbox-server"]
CMD ["--config", "/etc/opensandbox/opensandbox.toml"]
