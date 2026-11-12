# Reporting a wait

A node or a command can report what it waits for. The task row shows that text and
its start time while the task is working. A report does not change the task's
state, its slot, or admission. Alondra does not interpret the text.

The host sets `ALONDRA_WAIT_FILE` to `<host directory>/wait.json` for the node.
Use the CLI from a node or a command that inherits its environment:

```sh
alondra report-wait 'the deployment' --progress '2 of 3' --detail 'Waiting for acknowledgement'
alondra report-wait 'the server' --pid $$
alondra report-wait --clear
```

A script passes `--pid $$` so its report expires when the script exits. A node
omits `--pid`: its report lasts until cleared, replaced, or its turn ends. The
daemon deletes the file when it records the end of that turn. A PID names a
process on the host's machine. Without `ALONDRA_WAIT_FILE`, reporting or clearing
silently succeeds without writing anything, so scripts work outside Alondra.

## File format

The file holds one JSON object, at most 4096 bytes, encoded as UTF-8:

```json
{"what":"the deployment","detail":"Waiting for acknowledgement","progress":"2 of 3","since_ms":1794324000000,"pid":4242}
```

`what` is a nonempty string and `since_ms` is an integer Unix timestamp in
milliseconds. Both are required. `detail` and `progress` are optional strings;
`pid` is an optional positive process ID. Extra fields are ignored. Writers must
write a temporary file beside the destination and rename it over `wait.json` so
readers never see a partial update. The CLI does this for you, with a private
temporary file, and rejects reports larger than 4096 bytes.

Alondra ignores malformed files, missing required fields, empty `what`, files
larger than 4096 bytes, and reports whose PID is no longer alive. For display it
replaces line breaks and control characters with spaces and limits `what` to 60
characters, `progress` to 40, and `detail` to 120, without splitting Unicode
characters. Empty optional texts are omitted.

The row reads “Waiting for {what} · {progress} · {detail} · since {time}” in English
and “Espera a {what} · {progress} · {detail} · desde las {hora}” in Spanish, omitting
absent fields. The time comes from `since_ms`. This report takes precedence over
the command or background work in the row. Without a valid report the row shows
the current command, background work, or running turn as usual.
