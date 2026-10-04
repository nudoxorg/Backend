# aarch64 lane: the `platform` test selection for aarch64-unknown-linux-gnu
# under QEMU user mode, invoked as `nu .config/ci/arm64-emu.nu` inside
# `.#arm64-emu`.

use lib.nu [emulated-lane]

def main []: nothing -> nothing {
    emulated-lane "arm64-emu" "arm64"
}
