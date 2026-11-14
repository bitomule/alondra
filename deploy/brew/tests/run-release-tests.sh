#!/usr/bin/env bash
set -euo pipefail
suite=$1
shift
exec python3 -m unittest discover -s "$(dirname "$suite")" -p "$(basename "$suite")" -v "$@"
