#!/bin/sh
set -eu

if [ "$#" -lt 2 ]; then
    cat >&2 <<'USAGE'
Usage: observe-rust-ra-reads.sh OUTPUT_PREFIX AUTHORITY_TEST_EXECUTABLE [TEST_ARGS...]

Capture filesystem system calls for one already-built rust-analyzer authority
test process and Cargo/rustc child commands. The test process is stopped before
exec so fs_usage can attach before the test starts.
USAGE
    exit 64
fi

output_prefix=$1
shift
command_log="${output_prefix}.authority.log"
trace_log="${output_prefix}.fs_usage.log"
metadata_log="${output_prefix}.metadata.txt"
output_parent=$(dirname "$output_prefix")
mkdir -p "$output_parent"

if [ "$(uname -s)" != Darwin ] || [ ! -x /usr/bin/fs_usage ]; then
    printf '%s\n' 'This experiment requires macOS /usr/bin/fs_usage.' >&2
    exit 69
fi

# fs_usage uses the kernel tracing facility and requires administrator rights.
sudo -v

sh -c 'kill -STOP "$$"; exec "$@"' ra-read-frontier-observer "$@" \
    >"$command_log" 2>&1 &
authority_pid=$!
observer_pid=

cleanup() {
    if [ -n "$observer_pid" ]; then
        sudo -n kill -INT "$observer_pid" >/dev/null 2>&1 || true
        wait "$observer_pid" >/dev/null 2>&1 || true
    fi
    if kill -0 "$authority_pid" >/dev/null 2>&1; then
        kill -CONT "$authority_pid" >/dev/null 2>&1 || true
        kill -TERM "$authority_pid" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT HUP INT TERM

{
    printf 'authority_pid=%s\n' "$authority_pid"
    printf 'command='
    for argument in "$@"; do
        printf '%s ' "$argument"
    done
    printf '\n'
    printf 'captured_processes=authority pid %s plus processes named cargo and rustc\n' "$authority_pid"
} >"$metadata_log"

# Process-name filters include Cargo's metadata/locate-project subprocesses and
# rustc toolchain queries. The test process itself is selected by its exact PID.
sudo -n /usr/bin/fs_usage -w -f filesys -f pathname \
    "$authority_pid" cargo rustc >"$trace_log" 2>&1 &
observer_pid=$!

# Allow fs_usage to install its kernel trace before the stopped authority resumes.
sleep 1
if ! kill -0 "$observer_pid" >/dev/null 2>&1; then
    printf '%s\n' 'fs_usage exited before the authority test could start; inspect the trace log.' >&2
    exit 70
fi
kill -CONT "$authority_pid"

set +e
wait "$authority_pid"
authority_status=$?
set -e

sudo -n kill -INT "$observer_pid" >/dev/null 2>&1 || true
wait "$observer_pid" >/dev/null 2>&1 || true
observer_pid=
trap - EXIT HUP INT TERM

printf 'authority exit status: %s\n' "$authority_status"
printf 'authority output: %s\n' "$command_log"
printf 'filesystem trace: %s\n' "$trace_log"
printf 'capture metadata: %s\n' "$metadata_log"
exit "$authority_status"
