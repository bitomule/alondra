---
type: llm
weight: 3
focus: last_message
---

The answer ends with the mission the orchestrator gave the research task. PASS only if that
mission says both:
- if the research proposes nothing to build, it delivers the report alone (no plan);
- if it concludes that something should be built, it delivers one plan card (the work as the
  result's plan, which Alice approves or sends back with changes, with the report beside it), and
  asks any decision the plan leaves open before delivering it.
FAIL if the mission asks for a plan (phased or not) written inside the report, if it says the plan
is approved by accepting the report, if it offers to only archive the plan, or if it says only one
of the two endings.
