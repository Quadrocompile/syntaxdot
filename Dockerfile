FROM nvidia/cuda:11.1-base

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl netbase unzip wget

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    RUST_VERSION=stable

RUN set -eux; \
    dpkgArch="$(dpkg --print-architecture)"; \
    case "${dpkgArch##*-}" in \
        amd64) rustArch='x86_64-unknown-linux-gnu'; rustupSha256='49c96f3f74be82f4752b8bffcf81961dea5e6e94ce1ccba94435f12e871c3bdb' ;; \
        armhf) rustArch='armv7-unknown-linux-gnueabihf'; rustupSha256='5a2be2919319e8778698fa9998002d1ec720efe7cb4f6ee4affb006b5e73f1be' ;; \
        arm64) rustArch='aarch64-unknown-linux-gnu'; rustupSha256='d93ef6f91dab8299f46eef26a56c2d97c66271cea60bf004f2f088a86a697078' ;; \
        i386) rustArch='i686-unknown-linux-gnu'; rustupSha256='e3d0ae3cfce5c6941f74fed61ca83e53d4cd2deb431b906cbd0687f246efede4' ;; \
        *) echo >&2 "unsupported architecture: ${dpkgArch}"; exit 1 ;; \
    esac; \
    url="https://static.rust-lang.org/rustup/archive/1.22.1/${rustArch}/rustup-init"; \
    wget "$url"; \
    echo "${rustupSha256} *rustup-init" | sha256sum -c -; \
    chmod +x rustup-init; \
    ./rustup-init -y --no-modify-path --profile minimal --default-toolchain $RUST_VERSION --default-host ${rustArch}; \
    rm rustup-init; \
    chmod -R a+w $RUSTUP_HOME $CARGO_HOME; \
    rustup --version; \
    cargo --version; \
    rustc --version;

WORKDIR /opt

RUN wget https://download.pytorch.org/libtorch/cu102/libtorch-cxx11-abi-shared-with-deps-1.6.0.zip
RUN unzip libtorch-cxx11-abi-shared-with-deps-1.6.0.zip

ENV LIBTORCH=/opt/libtorch

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get install -y build-essential cmake pkg-config libhdf5-dev libssl-dev

WORKDIR /usr/src
ADD Cargo.toml Cargo.lock syntaxdot-encoders syntaxdot-transformers syntaxdot syntaxdot-cli ./
RUN cargo build --release

RUN apt-get install -y patchelf

RUN install -Dm755 -t /opt/syntaxdot target/release/syntaxdot
RUN install -Dm755 -t /opt/syntaxdot ${LIBTORCH}/lib/*.so*
RUN patchelf --set-rpath '$ORIGIN' "/opt/syntaxdot/syntaxdot"

ENTRYPOINT [ "/opt/syntaxdot/syntaxdot" ]