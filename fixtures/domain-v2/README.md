# Synthetic domain v2 specifications

These files contain no production identity, token, endpoint or account data.
They specify expected behavior; **none records an executed product test**.

- `acceptance.json`: all 53 acceptance scenarios and R01–R16 coverage. Base and
  named variants describe semantic arrangements; they are not a serialized
  `DomainState`. Variant `extends` merges the named setup, then overrides its
  fields. Scenario `arrange` entries apply in order before recording deltas.
- `fixture.schema.json`: JSON Schema 2020-12 for the scenario catalog.
- `command-envelope.schema.json`: exact P1 command envelope and tagged command
  shapes, with unknown fields rejected. These match the P1 declarations read
  during P0; production guards remain responsible for identity, revision,
  state, authorization and relation checks that a JSON schema cannot express.
- `text-completion.commands.json`: deterministic envelopes from an empty epoch
  through text presentation, acceptance and explicit completion. Actor names
  are **test harness principal aliases**, not fields accepted from clients.

## Adapter requirements

For catalog command steps, construct a unique command_id from scenario + step,
set protocol_version 2/data_epoch from base, and use the current revision unless
an environment step explicitly tests stale revisions or retries. Preserve the
original ID/envelope for retries. Sequential commands continue after an expected
rejection with the unchanged revision. Setup is not counted in `delta.*`.

A `command` step uses the P1 wire payload. An `environment` step is an explicit
harness operation (concurrency, SQL fault injection, external fake, restart,
client UI action); it is never sent to the product as a new endpoint. Each adapter
must implement or report missing support, never skip it as success. Semantic
assertion paths describe measured adapter outputs, not assumed wire field paths.
For example `tasks.task-1.status` maps to P1 Task.state; `delta.rows` counts all
relevant durable mutations, and `delta.messages/outbox/delivery_attempts` must be
queried/countable even when the expected count is zero.
`presentations.<id>.acceptance_recorded` observes the historical decision;
`presentations.<id>.acceptance_valid` observes whether that decision is still
usable for completion. A new final presentation or feedback can make the latter
false without erasing the former.

Fixtures with `https://...example.invalid` are references only. No external
request should be made to them. `head-a`, `sha256-v1` and similar values are
synthetic symbolic observations; tests involving digest validation must compute
real digests from the given synthetic content. Never interpret them as valid
production SHA values or weaken production validators to accept a fixture.

The text command trace expects exactly one revision per successful mutation.
Review its expected values if P1 chooses a documented no-op policy, rather than
blindly regenerating expectations. Canonical projection JSON is added in P3 by
serializing the actual Rust DTO; Swift/TUI parity cannot be asserted from this
semantic catalog alone.

## Execution evidence

Write results outside these input specifications, recording source SHA, dirty
flag, adapter/layer, actual assertions, doubles and resource cleanup. A catalog
parser or schema check proves fixture structure only. Do not mark V01–V53 or P7
passed until their required layers have actually run.

Every scenario carries an `execution_status` from `execution_status_legend`.
`domain_store::acceptance_tests` executes the `store_runner` scenarios against
fresh SQLite stores: it sends the catalog's command steps and the observation
environment steps (`observe_new_head`, `observe_merge`, as the trusted adapter
would) through the production domain command/store path, compares every listed
assertion except client-only paths (`client.*`, `clients.*`, `ui.*`, skipped
explicitly), checks rejected writes and SQL integrity, then reloads each store.
`schedule_runner` scenarios run in `domain_store::schedule_acceptance_tests`.
`test_evidence` scenarios name the Rust tests that cover them in `cs_evidence`;
`store_pending`, `producer_pending` and `ui_pending` say which layer is missing.
`catalog_execution_status_is_explicit_and_evidenced` checks that every status
matches the runner lists and that every evidence name is an existing test. All of
this is C/S coverage only: it does not exercise HTTP, the encrypted relay,
clients or Claude.
