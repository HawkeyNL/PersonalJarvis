# Coding Agent Security Checklist

- [ ] Trust boundary documented
- [ ] Authentication appropriate
- [ ] Capability in access-control matrix
- [ ] Ownership and deny-by-default tested
- [ ] Step-up/approval for sensitive actions
- [ ] No hardcoded/client-side secrets
- [ ] Rotation/revocation documented
- [ ] Runtime schemas and size/range limits
- [ ] Rate-limit profile and timeouts
- [ ] Bounded retries/concurrency
- [ ] Idempotency for consequential writes
- [ ] Safe errors and logging
- [ ] Unit/integration/security tests
- [ ] Injection/SSRF/path traversal tests where relevant
- [ ] Secret-leak test
