---
name: spec-validator-finds-a-weak-test
description: The validator of a task with specs judges that a test which checks nothing its scenario says does not verify it, and records it as unmet with its reasoning
tags: [skill, specs]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-refunds, as its validator: the task goes in stages and
  another agent implemented it. Load skills with the Skill tool; no other tool runs in this run.
  Write, as your final answer, the Alondra tool call you would make now, with its exact tool name
  and every argument as JSON, and nothing else.
---

Load the spec-validation skill, then do the next step of your task.

Your context lists `billing.partial/part` (adequate) as owed. The scenario says:

```gherkin
@id:part
Scenario: Part of an order
  Given a paid order of 30 EUR
  When 10 EUR are refunded
  Then the customer receives 10 EUR
  And 20 EUR are left to refund
```

The only test that declares it, in `src/billing/tests/refunds.rs`:

```rust
// spec: billing.partial/part
#[test]
fn refunds_part_of_an_order() {
    let mut order = Order::paid(3000);
    assert!(order.refund(1000).is_ok());
}
```

The suite `billing/unit` already passed and you recorded it.
