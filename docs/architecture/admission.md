# Admission of agents

The gate (`crates/daemon-engine/src/admission.rs`) decides when a waiting task's node starts or a
paused one comes back. It reads what an **agent** spends, which is memory, and nothing a build spends.

| Check | Holds when | Source |
| --- | --- | --- |
| Slots | nodes at work reach the limit | `max_working_nodes`: a number is the most that may work; `auto` is the memory over 4 GiB divided by the measured cost of a node, up to two per core |
| Memory | macOS reports critical pressure, free memory cannot take the node that starts and the ones admitted in the last 3 minutes, or macOS swaps out while free memory is low | `MacState::room` |
| Spacing | the last start was under a minute ago (3 minutes under macOS's warning) | `Tuning::spacing` |
| Disk | the disk has no room for the node's build on top of what the nodes at work may still take | `disk_peaks`, `swap_disk` |

The CPU load is shown and recorded in the machine samples, and holds nothing: running tools spend the
CPU, and each tool limits its own work.

A node whose turn ended and only waits for work of its own to finish takes no slot. Its memory is
still measured in the node cost, and it needs no admission to wake up when the work ends.
