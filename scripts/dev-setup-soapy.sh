#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Nexus-BS contributors
# SPDX-License-Identifier: Apache-2.0 AND PolyForm-Noncommercial-1.0.0
#
# Bootstrap a local SoapySDR build for dev machines without libsoapysdr-dev
# (no root needed). Builds into the git-ignored deps/ directory.
#
# Usage:
#   scripts/dev-setup-soapy.sh
#   source deps/env.sh
#   cargo test
#
# If `pkg-config --exists SoapySDR` already succeeds (e.g. apt's
# libsoapysdr-dev), this script is not needed at all.

set -euo pipefail
cd "$(dirname "$0")/.."

if pkg-config --exists SoapySDR 2>/dev/null; then
    echo "SoapySDR already available via pkg-config: $(pkg-config --modversion SoapySDR)"
    echo "Nothing to do."
    exit 0
fi

command -v cmake >/dev/null || { echo "error: cmake is required" >&2; exit 1; }

if [ ! -d deps/SoapySDR ]; then
    git clone --depth 1 https://github.com/pothosware/SoapySDR.git deps/SoapySDR
fi

cmake -S deps/SoapySDR -B deps/soapy-build -G "Unix Makefiles" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$PWD/deps/soapy-install" \
    -DSOAPY_SDR_PYTHON=OFF \
    -DSOAPY_SDR_EXAMPLES=OFF \
    -DSOAPY_SDR_UTILS=OFF \
    -DSOAPY_SDR_TOOLS=OFF \
    -DBUILD_SHARED_LIBS=ON
cmake --build deps/soapy-build -j"$(nproc)"
cmake --install deps/soapy-build >/dev/null

if [ ! -f deps/env.sh ]; then
    cat > deps/env.sh <<'EOF'
export PKG_CONFIG_PATH="$PWD/deps/soapy-install/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LIBRARY_PATH="$PWD/deps/soapy-install/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$PWD/deps/soapy-install/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
EOF
fi

echo "Done. Run: source deps/env.sh"
