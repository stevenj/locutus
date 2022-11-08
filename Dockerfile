FROM node:18-bullseye-slim

RUN DEBIAN_FRONTEND=noninteractive \
  && apt-get update \
  && apt-get upgrade -yqq \
  && apt-get install --no-install-recommends -y \
    build-essential \
    curl \
    ca-certificates \
    llvm-13 \
    llvm-13-dev \
    libclang-common-13-dev \
    zlib1g-dev \
  && apt-get clean \
  && rm -fr /var/lib/apt/lists/*

ENV PATH="/usr/lib/llvm-13/bin/:${PATH}"

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# Go to a non-root Work directory so npm install works.
WORKDIR /src/locutus

# NodeJS/NPM Dependencies
RUN npx --package typescript tsc --init
RUN npm install --global webpack
RUN npm install --global webpack-cli
RUN npm install @msgpack/msgpack
RUN npm install bs58

# Copy the locutus source
COPY . .

# Build it
WORKDIR /src/locutus/crates/locutus-node
RUN cargo install --path . 
 
CMD ["locutus-node"]