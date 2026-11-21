# Reading web pages in chat

When the owner's chat message links to a page, Core can read that one page
and let the chat model answer from it. Core never fetches the page itself:
the separate `jarvis-page-reader` service does, as its own `jarvis-reader`
account, and returns only the title and plain text over its local Unix
socket. Core refuses a socket that is not served by that account.

It is **off by default** (`JARVIS_WEB_FETCH_ENABLED=false`), and no install
or update enables the reader unit. While it is off, or the `jarvis-reader`
account does not exist, Core reads no page and the model is not told about
pages.

## Enabling it (owner)

1. Start the reader: `sudo systemctl enable --now jarvis-page-reader.service`.
2. Set `JARVIS_WEB_FETCH_ENABLED=true` in `/etc/jarvis/core.env` and restart
   Core. `reading web pages stays off` in the Core log at startup means the
   reader account is missing or unsafe; Core keeps running either way.
3. If the Home Node's firewall lists allowed outgoing hosts, the reader needs
   TCP 443 out for uid `jarvis-reader` only.

To turn it off, set the flag back to `false`, restart Core and
`sudo systemctl disable --now jarvis-page-reader.service`.

## Which page is read

- Only on an ordinary chat turn through the runs API: never in Research
  mode, never on the old chat endpoint.
- Only a URL the owner gave: the first `https://` link in the owner's own
  message, or otherwise "source N" / "bron N" (1 to 20) of the newest
  Research answer in the conversation. The chat model can never choose a
  URL: its context holds memory, facts and portfolio data that a URL could
  carry out. A page answer's own links are never a "source N".
- One page per turn, at most 20 an hour and 100 a day (one at a time).

## What is refused

Core checks the URL before anything is sent, and the reader checks it again
at every redirect:

- https on port 443 only, a public domain name (no IP literal, no
  single-label or special-use name such as `localhost` or `.local`), no user
  info, and not Core's own `JARVIS_PUBLIC_HOSTNAME` or a name below it;
- no path or query that looks like an action (`logout`, `unsubscribe`,
  `delete`, `confirm`, `?action=`…);
- in Core only: no email address, key-like text, phone or account number in
  the link, and no credential-like query name (`token`, `key`, `sig`,
  `session`, `code`, `state`…).

The reader resolves the name itself, refuses it when any address is not
public (private, loopback, link-local, CGNAT, documentation, multicast,
reserved, the IPv4 inside mapped, NAT64 and 6to4 addresses, and the networks
of the Home Node's own interfaces), connects only to the checked addresses,
follows at most 3 redirects, sends no cookies, auth or Referer and uses no
proxy. It reads only `2xx` `text/html`, `application/xhtml+xml` or
`text/plain`, at most 2 MiB within 15 seconds, and drops scripts, styles,
navigation, forms and hidden characters. Its unit also denies those address
ranges (`IPAddressDeny=`) and any IP listener.

A refused or failed read is not an error: the model gets a fixed note and
tells the owner the page was not read. The reader's journal records the host
and the outcome only.

## How the answer is stored

- The page text goes to the model once, fenced as untrusted data before the
  owner's question. It is **not stored**; the owner's message is stored as
  typed.
- On a page turn the answer is not streamed, every marker (web search, jobs,
  roadmaps, photos) is removed and none is honoured, no web search or search
  offer follows, remote images become their alt text and a `Sources` line
  with the page's final URL is added.
- The answer is stored with model `read:<model>`. Later turns carry it as
  untrusted data, like a `web:` Research answer.
