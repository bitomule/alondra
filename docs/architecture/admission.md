# Admission of agents

The gate (`crates/daemon-engine/src/admission.rs`) decides when a waiting task's node starts or a
paused one comes back. It measures the system's available memory and swap, and the memory of
Alondra's nodes. Other programs affect the system totals; the daemon does not classify them.

After a restart, starts and resumes wait for valid budgets of memory and the system disk. Memory
needs a positive total and available bytes no greater than that total; zero available bytes hold admission.
A percentage, pressure level, swap counter or healthy external disk cannot stand in for those
budgets. A first healthy measurement is enough, without a fixed delay at startup. Optional percentage,
pressure and swap readings need not be present.

Invalid readings retain the last valid budgets and cannot establish recovery or include newer node
footprints in an older memory budget. Disk readings require a positive total and free plus purgeable
space no greater than that total. The system disk remains part of admission even when another
volume is the fullest one. Fast sandboxes bypass memory explicitly for their fake nodes; their
slot and spacing checks still apply.

Independent pressure, low free percentages and swap activity can tighten admission even when the
byte budget is missing. Such readings cannot clear a restriction or count toward swap recovery.
Unknown, incoherent or reset swap counters retain a known swapping floor until a fresh minute of
valid counters establishes recovery.

Learned build volumes retain their last valid space budget and recovery margin when a probe fails.
Reservations against each budget remain after work ends, until a valid space reading accounts for
its writes. Every successful admission records its estimated writes, including work that finishes
between samples. Repeated queries of one admission add no charge. A new admission cannot erase
earlier writes or reuse older consumption credit, even for the same task. Forecasts exclude the
task at the front of the queue and turns held for disk; earlier grants remain counted.

An error reading the mount table cannot confirm an unmount, and cached space cannot establish
incident recovery. A complete table suspends the restriction of an absent volume without forgetting
its learned peaks. The aggregate budget and incident retain the mount that caused them, so confirmed
absence retires that volume's hold even if the system probe fails. The system disk's own recovery
margin and other volumes' restrictions remain independent. Positive mount evidence supersedes an
older confirmed absence, even if the next mount-table read fails. A remounted volume waits for a
valid reading of its own filesystem; space from the directory's parent filesystem cannot replace it.
Launch preflight also waits for that budget, including a launch admitted before the volume appeared.
This applies to every learned build volume, even when the project's directory is on the system disk.
Its immediate space probe cannot publish a new budget or discard reservations; the watcher measures
the volume's identity and publishes its own coherent budget before preflight can proceed.
Disk preflight refusals invalidate the unused admission, so retries evaluate and reserve against
the current budgets instead of reusing a grant made while the volume was absent.
Waiting for a first reading does not invent a measured shortage or an additional recovery margin.

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
