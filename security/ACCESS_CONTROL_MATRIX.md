# Access Control Matrix

Every API route, MCP tool and Device Mesh task references one capability.

Dimensions:
- actor and role;
- device trust;
- environment;
- ownership;
- scopes;
- step-up authentication;
- approval;
- risk decision;
- audit level;
- rate-limit profile.

```yaml
capability: broker.order.submit.live
actors:
  user: allow
  agent: deny_direct
trusted_device_required: true
step_up_required: true
approval_required: true
risk_decision_required: true
resource_rule: approved_account_and_proposal_only
audit: full_redacted
rate_limit_profile: critical_write
```

No wildcard allow. Matrix changes require security review. Production matrix hash is visible in the Observatory.
