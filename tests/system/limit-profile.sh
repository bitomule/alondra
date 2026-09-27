#!/bin/sh
# Only the disposable Docker build calls this with its copied source.
set -eu
profile="${1:?profile required}"
case "$profile" in
  baseline) exit 0 ;;
  tuned) changes=256; work=32; requests=384 ;;
  capacity-1000) changes=699; work=349; requests=1256 ;;
  capacity-5000) changes=3366; work=1682; requests=5256 ;;
  capacity-10000) changes=6699; work=3349; requests=10256 ;;
  *) exit 1 ;;
esac
file=crates/relay/src/main.rs
for literal in 'change_waiters: Arc::new(Semaphore::new(64))' 'work_waiters: Arc::new(Semaphore::new(16))' 'requests: Arc::new(Semaphore::new(128))'; do
  grep -Fq "$literal" "$file" || exit 1
done
sed -i "s/work_waiters: Arc::new(Semaphore::new(16))/work_waiters: Arc::new(Semaphore::new($work))/g; s/change_waiters: Arc::new(Semaphore::new(64))/change_waiters: Arc::new(Semaphore::new($changes))/g; s/requests: Arc::new(Semaphore::new(128))/requests: Arc::new(Semaphore::new($requests))/g" "$file"
