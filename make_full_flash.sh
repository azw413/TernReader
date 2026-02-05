#!/bin/bash
set -euo pipefail

OUT_FILE=${1:-trusty-full.bin}

cargo espflash save-image \
  --merge \
  --release \
  --chip=esp32c3 \
  --target=riscv32imc-unknown-none-elf \
  --package=trusty-x4 \
  "$OUT_FILE"

echo "Wrote merged flash image: $OUT_FILE"
