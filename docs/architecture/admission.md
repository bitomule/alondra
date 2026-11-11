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
| Slots | active nodes reach the limit | `max_working_nodes`: a number is the most that may work; `auto` is the memory over 4 GiB divided by the measured cost of a node, up to two per core |
| Memory | macOS reports critical pressure, available memory cannot cover the floor and pending nodes, or active swap grows | `MacState::room` |
| Spacing | the last start was under a minute ago (3 minutes under macOS's warning) | `Tuning::spacing` |
| Disk | the disk has no room for the node's build on top of what the nodes at work may still take | `disk_peaks`, `swap_disk` |

The CPU load is shown and recorded in the machine samples, and holds nothing: running tools spend the
CPU, and each tool limits its own work.

Nodes paused or waiting reserve no disk: only the nodes at work, the admissions since the last
disk reading and the next node do. When nothing but the next node reserves a disk, it needs only
that disk's line (5 GB, plus the 2 GB recovery margin once the disk was short), whatever its
estimate, so one node can always work while the disk stays over its line. The floor that holds the
next turns of all but the lightest node and the disk card remain the backstop if it fills the disk.

## What counts as an active node

`max_working_nodes` limits active nodes. The gate, the Tasks list and the header share one count:

| Counts | Does not count |
| --- | --- |
| Starting, or brought back and restarting | Waiting for the user |
| Working: a turn runs, or a turn it runs after delivering | Paused |
| In the background: its turn ended on work of its own | Waiting for another task |
| A turn the gate gave a slot | Resting, or waiting for capacity |
| | Closing |

A node in the background keeps its process and its memory, and it wakes without passing the gate,
so it keeps its slot. A delivered node whose leftover work still runs waits for the user and takes
none.

A node that waits for an external queue, such as the build queue, counts. It is a live process
running a command, and the daemon has no typed fact that tells that command from any other work. With
8 as the limit, 8 nodes waiting for builds fill it, and the memory stays for the builds they wait
for. Work in the background that hangs holds its slot until its node closes.

## Turns pass the gate

Whatever brings a node back to work ends in one place: its owner hands an input to the provider, or
the answer to a decision its open turn holds (`Driver::step_at`). Before that, the owner asks
`Admission::may_take_turn`. This covers an answer, a message, a handback of a pull request, the end of a wait
for a task, a machine, a usage limit or a server, a wake after an empty turn, and any retry. A new
host covers it too: adoption, a replacement, a move of account or provider, because a host opens its
session on its first input.

What does not fit stays as the task's pending input, with no card and no slot, and the task reads
«Waiting for capacity» with its place in the queue. The queue comes from the domain's state
(`DomainState::waits_for_turn`), not from who asked: an input or an answer waits there whether or not
its owner is looking, and asking only lets it go on in the same look when there is room. A turn whose
answer has not reached it yet holds no slot while it waits for one (`TaskLifecycle::holds_slot`). The
gate gives the slot when one frees, by priority and then age, in the same queue as starts and
resumes, and wakes the owner; a task with no owner in this daemon is taken over. The slot counts from
then until what waited reaches the node; something that waits after it queues again. The age lives
in `admission.json`.

A turn only needs a slot. Neither memory nor the spacing of starts holds it: its process exists or
costs little to resume, and the user's answers do not go out one a minute. The first start of the
queue, when it is ahead of a turn, keeps a slot if only the spacing holds it. A start memory or the
disk holds keeps none, so what cannot use a slot never holds back what can.

Checking and reserving happen under the gate's lock on a state read under it. A reservation has no
timer: it holds while its node waits and its host lives, and then the node counts by its own state. A
resume the gate let through carries its slot over to the turn it starts.

These go on without asking:

- An input to a running turn, which already holds its slot.
- The orchestrator, which the limit never counted.
- A task that is closing: cleaning up frees what the others wait for.
- Controls other than an answer, such as interrupting or stopping a node.
- A turn Claude starts by itself, as when a subagent ends. It counts as soon as it is seen, and
  nothing else enters while the count is over the limit.
- Turns already running when a daemon adopts their hosts, or when the limit goes down. A working
  node is never cut, so the queue waits for the count to fall.
- A `claude --bg` node left paused by an older daemon that the user writes to: the message already
  went down its connection. These nodes move to the Agent SDK as soon as they rest.

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
