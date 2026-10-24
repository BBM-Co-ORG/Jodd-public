#!/bin/sh
# Builds the throwaway OpenSSH container, runs the #[ignore] Docker tests
# against it, and always removes it. Local only; CI does not run this.
# The container is bound to 127.0.0.1 and its password is test data.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
docker build -q -t jodd-ai-host-test "$here" >/dev/null
docker rm -f jodd-ai-host-test >/dev/null 2>&1 || true
# --init: a reaper as PID 1, so a killed process is gone rather than a zombie
# that `kill -0` would still report as alive.
docker run -d --rm --init --name jodd-ai-host-test -p 127.0.0.1:2222:22 jodd-ai-host-test >/dev/null
trap 'docker rm -f jodd-ai-host-test >/dev/null 2>&1 || true' EXIT
sleep 2
cd "$here/../../src-tauri"
# macOS with an unaccepted Xcode license: prefix DEVELOPER_DIR=/Library/Developer/CommandLineTools
JODD_AI_DOCKER=127.0.0.1:2222 cargo test -p jodd --lib llm::remote::docker_tests -- --ignored --nocapture --test-threads=1
