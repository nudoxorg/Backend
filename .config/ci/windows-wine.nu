# Windows lane: the `platform` test selection for x86_64-pc-windows-gnu under
# Wine, invoked as `nu .config/ci/windows-wine.nu` inside `.#windows-wine`.

use lib.nu [emulated-lane]

def main []: nothing -> nothing {
    emulated-lane "windows-wine" "windows"
}
