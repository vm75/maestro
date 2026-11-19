FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# Install host compilation packages required by the Entware buildroot
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential git curl wget ca-certificates gnupg python3 python3-setuptools \
    perl rsync subversion unzip zip file gawk gettext flex bison cpio bc \
    libncurses-dev libssl-dev zlib1g-dev libelf-dev libxslt1-dev \
    pkg-config time patch diffutils util-linux xz-utils zstd cmake ninja-build swig \
    autoconf automake libtool libtool-bin texinfo sudo \
    && rm -rf /var/lib/apt/lists/*

# Entware buildroot forbids building as root.
# Create a builder user with UID 1001 (matching GitHub Actions runner).
RUN groupadd -g 1001 builder 2>/dev/null || true && \
    useradd -m -s /bin/bash -u 1001 -g 1001 builder 2>/dev/null || useradd -m -s /bin/bash -u 1001 builder && \
    echo "builder ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/builder && \
    chmod 0440 /etc/sudoers.d/builder

# Configure git safe directory globally
RUN git config --system --add safe.directory '*'

# Prepare toolchain directory owned by builder
RUN mkdir -p /opt/nginx-entware && chown -R builder:builder /opt/nginx-entware

USER builder
WORKDIR /opt/nginx-entware

# Copy build script into container to pre-build the cross-toolchain
COPY --chown=builder:builder tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh /tmp/build-toolchain.sh

ARG JOBS=4
RUN bash /tmp/build-toolchain.sh --toolchain-only --jobs "${JOBS}" --workdir /opt/nginx-entware \
    && rm -f /tmp/build-toolchain.sh

WORKDIR /workspace
