#!/bin/sh
# Download and execute the immutable GitHub diagnostic-preview bootstrap as a saved file.
# The current GitHub channel remains explicitly diagnostic and records known limits.

nudox_install_main() {
    set -u
    umask 077
    installer_url='https://github.com/nudoxorg/Backend/releases/download/nudox-preview-installer/install-bootstrap-c533dfe34b7d137a.py'
    installer_sha256='c533dfe34b7d137a08f1569e217cdfd44fc733c8584f0b619c57a23b27cc0209'
    installer_bytes=6033

    case "${1-}" in
        -h|--help)
            printf '%s\n' 'Usage: sh install.sh [--prefix PATH] [--allow-downgrade]' \
                'Requires curl with HTTPS support and CPython 3.8 or newer.' \
                'Supports macOS arm64 and Linux x86_64; installs the current GitHub diagnostic preview.' \
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
    python_probe='import fcntl, hashlib, json, os, pathlib, platform, subprocess, sys, tarfile, urllib.request; sys.exit(0 if platform.python_implementation() == "CPython" and sys.version_info >= (3, 8) and all(hasattr(os, name) for name in ("O_NOFOLLOW", "O_NONBLOCK", "geteuid")) else 1)'
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
    printf '%s\n' 'Downloading the pinned GitHub diagnostic-preview bootstrap.' >&2
    # The admitted CPython streams curl into an exclusive file and enforces the
    # exact byte ceiling itself. Older curl versions do not apply --max-filesize
    # to all in-progress/chunked transfers, so the curl option is only advisory.
    bounded_download='
import hashlib,os,subprocess,sys
limit=int(sys.argv[4])
try:
 fd=os.open(sys.argv[2],os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW|os.O_CLOEXEC,0o600)
except OSError as error:
 print("nudox installer: cannot create private bootstrap file: "+str(error),file=sys.stderr)
 sys.exit(1)
command=["curl","-q","--fail","--silent","--show-error","--location","--proto","=https","--proto-redir","=https","--tlsv1.2","--connect-timeout","15","--max-time","180","--max-filesize",str(limit),"--output","-",sys.argv[1]]
try:
 process=subprocess.Popen(command,stdout=subprocess.PIPE)
except OSError as error:
 os.close(fd)
 print("nudox installer: cannot start curl: "+str(error),file=sys.stderr)
 sys.exit(127)
digest=hashlib.sha256()
total=0
oversized=False
try:
 with os.fdopen(fd,"wb") as output:
  while True:
   chunk=process.stdout.read(min(65536,limit-total+1))
   if not chunk: break
   if len(chunk)>limit-total:
    oversized=True
    process.kill()
    break
   output.write(chunk)
   digest.update(chunk)
   total+=len(chunk)
 process.stdout.close()
 status=process.wait()
except BaseException:
 process.kill()
 process.stdout.close()
 process.wait()
 raise
if oversized:
 print("nudox installer: downloaded bootstrap exceeded its pinned byte count; the installer was not executed.",file=sys.stderr)
 sys.exit(1)
if status:
 sys.exit(status)
if total!=limit or digest.hexdigest()!=sys.argv[3]:
 print("nudox installer: downloaded bootstrap size/SHA-256 mismatch; the installer was not executed.",file=sys.stderr)
 sys.exit(1)
'
    if "$install_python" -c "$bounded_download" \
        "$installer_url" "$installer_path" "$installer_sha256" "$installer_bytes"; then
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
