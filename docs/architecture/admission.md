# Admission of agents

The gate (`crates/daemon-engine/src/admission.rs`) decides when a waiting task's node starts or a
paused one comes back. It measures the system's available memory and swap, and the memory of
Alondra's nodes. Other programs affect the system totals; the daemon does not classify them.

| Check | Holds when | Source |
| --- | --- | --- |
| Slots | nodes at work reach the limit | `max_working_nodes`: a number is the most that may work; `auto` is the memory over 4 GiB divided by the measured cost of a node, up to two per core |
| Memory | macOS reports critical pressure, available memory cannot cover the floor and pending nodes, or active swap grows | `MacState::room` |
| Spacing | the last start was under a minute ago (3 minutes under macOS's warning) | `Tuning::spacing` |
| Disk | the disk has no room for the node's build on top of what the nodes at work may still take | `disk_peaks`, `swap_disk` |

The CPU load is shown and recorded in the machine samples, and holds nothing: running tools spend the
CPU, and each tool limits its own work.

A node whose turn ended and only waits for work of its own to finish takes no slot. Its memory is
still measured in the node cost, and it needs no admission to wake up when the work ends.

Available bytes must cover the 20% memory floor, the next node's estimated cost, and the unobserved
part of nodes admitted in the last three minutes. A recent node footprint taken before the system
sample replaces that part of its reserve; it is never subtracted again from available memory.
Duplicate references to one node receive one credit, bounded by its estimated cost. Stale or failed
footprint measurements leave the pending reserve in place.

The memory gate watches a minute of swap activity independently of the disk budget. At least
64 MiB of swap use, allocation growth while swap is in use, or swap-outs together with growth holds
new admissions even with plenty of disk space. Positive changes count separately, so a swapfile
shrinking cannot hide subsequent growth. Recovery needs a full minute of fresh samples without
growth and below the swap-out threshold. Missing allocation readings cannot release an existing
hold. A missing available-memory reading likewise cannot age a low byte budget out of its history.

Memory holds open no card and do not interrupt active nodes. Admission and recovery logs include
system and node memory, pending reserves, swap use and allocation, growth, and the disk budget.
The existing store retention also logs its allocated bytes before and after space recovery.
