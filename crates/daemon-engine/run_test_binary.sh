#!/bin/sh
binary=$1
shift
if [ -n "$TESTBRIDGE_TEST_ONLY" ]; then
    exec "$binary" "$TESTBRIDGE_TEST_ONLY"
fi
exec "$binary" "$@"
