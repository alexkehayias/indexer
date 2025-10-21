#!/bin/sh

BIN_PATH="$( cd -- "$(dirname "$0")" >/dev/null 2>&1 ; pwd -P )"
SCRIPT_PATH="$(dirname $BIN_PATH)"

/usr/bin/env -S /bin/zsh -c "cd $SCRIPT_PATH && cargo run chat" "$@"
