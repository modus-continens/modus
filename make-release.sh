#!/usr/bin/env bash

set -xe

COMMIT_HASH=$(git rev-parse HEAD)
SHORT_HASH=${COMMIT_HASH:0:10}
docker pull "ghcr.io/modus-continens/modus-buildkit-frontend:$SHORT_HASH"

MODUS_LIB_VERSION=$(cargo metadata --format-version 1 | jq -r '.packages[]|select(.name=="modus-lib").version')
MODUS_VERSION=$(cargo metadata --format-version 1 | jq -r '.packages[]|select(.name=="modus").version')
if [ "$MODUS_LIB_VERSION" != "$MODUS_VERSION" ]; then
  echo -e "\e[31;1mmodus-lib and modus versions do not match!\e[0m"
  exit 1
fi

cargo publish -p modus-lib
sleep 5 # otherwise cargo will not be aware of the newly published modus-lib on crates.io
cargo publish -p modus