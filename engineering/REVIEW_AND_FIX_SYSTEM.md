# Review and Fix System

## Rules

- Implementer cannot approve its own work.
- Reviewers search for counterexamples and failure modes.
- Findings contain severity, evidence, location and required remediation.
- High-severity findings block merge.
- Original reviewer verifies the fix.

## Fix loop

```text
Finding
→ Reproduce
→ Root cause
→ Fix proposal
→ Implementation
→ Regression test
→ Affected test suites
→ Re-review
→ Merge gate
```

Do not weaken or delete tests to hide failures.
