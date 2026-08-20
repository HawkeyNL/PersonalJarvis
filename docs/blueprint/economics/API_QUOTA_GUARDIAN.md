# API Quota Guardian

## Purpose

Track AI provider budgets, rate limits, subscription periods and reset times.

## Inputs

- internal token/cost ledger;
- provider usage APIs where officially available;
- billing exports/invoices;
- configured monthly hard and soft limits;
- provider rate-limit response headers;
- manually configured subscription reset date when no API exists.

## States

- healthy;
- soft limit;
- restricted;
- exhausted;
- waiting for reset;
- reset detected;
- provider unavailable.

## Behaviour

```text
usage < 80%
→ normal routing

usage >= 80%
→ warning and prefer cheaper models

usage >= 95%
→ block nonessential jobs

hard limit reached
→ pause provider-specific queued tasks
→ switch allowed tasks to fallback
→ record resume condition

reset verified
→ resume paused tasks according to policy
→ notify user
```

## Safety

Never assume a limit reset solely because the calendar changed. Verify via provider response, usage endpoint, successful budget probe or manual confirmation.

## Queued work

Each paused task stores:

- permitted fallback models;
- maximum delay;
- expiry;
- sensitivity;
- whether automatic resume is allowed.

Financial execution tasks are not paused and later automatically replayed.
