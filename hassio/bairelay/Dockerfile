ARG BUILD_FROM=ghcr.io/hassio-addons/base:20.1.1
FROM ${BUILD_FROM}

SHELL ["/bin/ash", "-o", "pipefail", "-c"]

ARG BAIRELAY_VERSION
ARG BAIRELAY_SHA256_AMD64
ARG BAIRELAY_SHA256_AARCH64
ARG TARGETARCH

RUN set -eu \
	&& case "$TARGETARCH" in \
		amd64)   SUFFIX="x86_64-linux";  SHA="$BAIRELAY_SHA256_AMD64" ;; \
		aarch64) SUFFIX="aarch64-linux"; SHA="$BAIRELAY_SHA256_AARCH64" ;; \
		*) echo "unsupported arch: $TARGETARCH"; exit 1 ;; \
	   esac \
	&& URL="https://github.com/mgc8/bairelay/releases/download/v${BAIRELAY_VERSION}/bairelay-v${BAIRELAY_VERSION}-${SUFFIX}.tar.gz" \
	&& curl -fsSL "$URL" -o /tmp/bairelay.tar.gz \
	&& echo "${SHA}  /tmp/bairelay.tar.gz" | sha256sum -c - \
	&& tar -xzf /tmp/bairelay.tar.gz -C /tmp \
	&& install -m 0755 /tmp/bairelay-*/bairelay /usr/local/bin/bairelay \
	&& rm -rf /tmp/bairelay*

COPY rootfs/ /
