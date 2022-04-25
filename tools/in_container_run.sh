#!/bin/sh

set -eux

export PATH="/root/.cargo/bin:${PATH}"

COMMAND=${@:-"build --release"}

cargo $COMMAND

# copy our cached binaries and output over to the host filesystem. This
# preserves the cached builds in the volume, but moves everything else over.
for d in release debug; do
    if test -d /cache/$d; then
        rsync -a \
            --exclude build --exclude deps --exclude incremental \
            --exclude '*.d' --exclude .fingerprint \
            /cache/$d /host/
    fi
done
