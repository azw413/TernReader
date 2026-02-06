#!/bin/bash
set -euo pipefail

cargo espflash save-image --release --chip=esp32c3 --target=riscv32imc-unknown-none-elf --package=tern-x4 firmware.bin
if [[ $(stat -s firmware.bin) -gt 6553600 ]]; then
    echo -e "\033[0;31m[ERROR] Firmware size exceeds OFW partition limit!"
    exit 1
fi
cargo espflash write-bin 0x10000 firmware.bin --monitor
