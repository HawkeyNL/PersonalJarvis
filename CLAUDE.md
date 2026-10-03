@AGENTS.md

# Claude Code — PersonalJarvis workflow

The rules in AGENTS.md (imported above) take precedence. This file only adds
how Claude Code works in this repository.

## Language
- Code, identifiers, comments, Markdown/docs, commit messages and PR
  descriptions are always in **English**.
- Talk to the owner in **Dutch** in chat.

## Roles
- **You (main session, Sonnet) are the orchestrator.** Break the task down,
  carry it out and check the result.
- **Reading and searching code:** spawn a subagent with model `sonnet`. It
  reports findings; you keep the conclusion, not the file dumps.
- **Planning and hard thinking:** spawn a subagent with model `opus` before you
  build. Do this for every non-trivial task, and always for architecture,
  security, concurrency, performance and changes that span several crates.
- **Review:** have `opus` review the diff of anything that touches security,
  auth, approvals, policy, the sandbox or the broker before you call it done.

## Ask first, then act
- Do exactly what was asked. No unrequested features, refactors,
  dependencies, files or config.
- Ideas are welcome: list them briefly under **Suggestions** and only build
  them after an explicit "yes".
- Always ask before:
  - adding a dependency or crate;
  - changing a schema or migration;
  - changing policy, approvals, auth, the sandbox or the broker;
  - deleting anything;
  - committing, pushing or opening a PR;
  - anything on the host outside the repo (systemd, firewall, Docker,
    packages).

## Double-check
Before you say something is done:
1. Read your own diff (`git diff`) in full. Only what is needed, with no debug
   leftovers or commented-out code.
2. Run the checks from AGENTS.md for what you touched.
3. For security or concurrency code: have `opus` review it and address what it
   finds.
4. Report what you ran and what came out. If something could not be verified,
   say so honestly instead of calling it done.

## Security
- When in doubt: fail closed and ask.
- Never read, log, print or commit secrets, tokens, keys or `.env` contents.
- Validate all input at the boundaries: API, IPC and sandbox artifacts. No
  `unwrap`/`expect` on external input.
- Do not add open ports, public endpoints or shell execution.
- Give new code the least privilege that works.

## Performance
- Watch hot paths: no needless clones or allocations, no blocking I/O in async
  code, and put limits and timeouts on queues, retries and I/O.
- Measure before optimizing. Do not make code more complex without evidence
  that it gets faster.

## Code
- Prefer simple and readable over clever. Follow the existing style and
  patterns of the crate.
- Keep changes small and focused: one topic per commit or PR.
- Use typed errors and clear names. Only comment where the "why" is not
  obvious.
- Write tests for new behavior and for every bug fix.

## Local instructions
Machine-specific notes (which host, what else runs on it) live in a local,
uncommitted `CLAUDE.local.md`.

## Communication
- Keep replies short and concrete.
- Start a task with a plan of a few lines.
- Finish with what changed, what was verified and what is still open.
