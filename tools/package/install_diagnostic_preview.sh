#!/bin/sh
# Download and execute the immutable R2 Python bootstrap as a saved file.
# This delivery repair still installs the October 6/8 diagnostic binaries.

nudox_install_main() {
    set -u
    umask 077
    installer_url='https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/install.py'
    installer_sha256='36f0ea62e3b49cadebcef0f89e735639530c0ba1686e62d4dcbfea96294174f5'
    installer_bytes=13426

    case "${1-}" in
        -h|--help)
            printf '%s\n' 'Usage: sh install.sh [--prefix PATH] [--allow-downgrade]' \
                'Requires curl with HTTPS support and CPython 3.8 or newer.' \
                'Supports macOS arm64 and Linux x86_64; installs the historical R2 diagnostic preview.' \
                'NUDOX_INSTALL_PYTHON may select an explicit CPython executable.'
            exit 0
            ;;
    esac
    system=$(uname -s) || exit 1
    machine=$(uname -m) || exit 1
    case "$system/$machine" in
        Darwin/arm64|Darwin/aarch64|Linux/x86_64|Linux/amd64) ;;
        *) printf 'nudox installer: unsupported host %s/%s; supports macOS arm64 and Linux x86_64 only.\n' "$system" "$machine" >&2; exit 1 ;;
    esac
    if ! command -v curl >/dev/null 2>&1; then
        printf '%s\n' 'nudox installer: curl is required for verified HTTPS downloads.' >&2
        exit 127
    fi

    # Use a full CPython implementation. Some PATH shims treat "python3 -"
    # as a REPL and lack filesystem APIs; never pass downloaded source on stdin.
    python_probe='import fcntl, hashlib, json, os, pathlib, platform, subprocess, sys, tarfile, urllib.request; assert platform.python_implementation() == "CPython" and sys.version_info >= (3, 8); assert all(hasattr(os, name) for name in ("O_NOFOLLOW", "O_NONBLOCK", "geteuid"))'
    install_python=''
    if [ -n "${NUDOX_INSTALL_PYTHON-}" ]; then
        if command -v "$NUDOX_INSTALL_PYTHON" >/dev/null 2>&1 && "$NUDOX_INSTALL_PYTHON" -c "$python_probe" >/dev/null 2>&1; then
            install_python=$NUDOX_INSTALL_PYTHON
        else
            printf '%s\n' 'nudox installer: NUDOX_INSTALL_PYTHON must name a working CPython 3.8+ executable with the required standard library.' >&2
            exit 1
        fi
    else
        for candidate in /usr/bin/python3 python3; do
            if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c "$python_probe" >/dev/null 2>&1; then
                install_python=$candidate
                break
            fi
        done
        if [ -z "$install_python" ]; then
            printf '%s\n' 'nudox installer: CPython 3.8+ with the required standard library is required; set NUDOX_INSTALL_PYTHON to its executable.' >&2
            exit 1
        fi
    fi

    download_dir=$(mktemp -d "${TMPDIR:-/tmp}/nudox-install.XXXXXXXX") || {
        printf '%s\n' 'nudox installer: could not create a private download directory.' >&2
        exit 1
    }
    trap 'rm -rf "$download_dir"' 0
    trap 'exit 129' 1
    trap 'exit 130' 2
    trap 'exit 143' 15
    installer_path=$download_dir/install.py
    printf '%s\n' 'Downloading the pinned R2 diagnostic installer (historical October 6/8 binaries).' >&2
    # -q is first: ignore user curl configuration. Fail before Python on every
    # HTTP/transport error, including a partial response; both hops require TLS.
    if curl -q --fail --silent --show-error --location \
        --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --connect-timeout 15 --max-time 180 --max-filesize "$installer_bytes" \
        --output "$installer_path" "$installer_url"; then
        :
    else
        status=$?
        printf '%s\n' 'nudox installer: HTTPS download failed; the installer was not executed.' >&2
        exit "$status"
    fi
    if "$install_python" -c 'import hashlib,sys; limit=int(sys.argv[3]); stream=open(sys.argv[1],"rb"); body=stream.read(limit+1); stream.close(); sys.exit(0 if len(body)==limit and hashlib.sha256(body).hexdigest()==sys.argv[2] else 1)' \
        "$installer_path" "$installer_sha256" "$installer_bytes"; then
        :
    else
        printf '%s\n' 'nudox installer: downloaded bootstrap size/SHA-256 mismatch; the installer was not executed.' >&2
        exit 1
    fi
    "$install_python" "$installer_path" "$@"
}

nudox_install_main "$@"
