# Usage counter

A Cloudflare Worker with a D1 database that counts how many copies of Afterclip are in use, and a dashboard that adds Sleekmeeter's count to show users across both apps. It runs at `afterclip.app/api/*`; the rest of the website is untouched.

Copies whose users said yes send this once a day (see `src-tauri/src/usage.rs`):

```json
{ "id": "32 random hex characters", "version": "1.6.0" }
```

The Worker keeps one row per ID per UTC day, with just those two values and the date. It doesn't store IP addresses or anything else, and deletes rows older than 400 days.

## Deploy (once)

Run these from this folder, signed in to the Cloudflare account that has afterclip.app. `npx wrangler login` opens your browser to sign in; nothing is saved in the repo.

```bash
npx wrangler login
```

```bash
npx wrangler d1 create afterclip-usage
```

Copy only the `database_id` it prints into `wrangler.toml`. Keep `binding = "DB"` as it is. Then:

```bash
npx wrangler d1 execute afterclip-usage --remote --file schema.sql
```

```bash
npx wrangler deploy
```

Check it with `https://afterclip.app/api/stats`.

The `SLEEKMEETER` service binding in `wrangler.toml` lets the dashboard call the `sleekmeeter-usage` Worker directly. It needs both Workers in the same Cloudflare account. If Sleekmeeter's Worker is elsewhere, delete the `[[services]]` block and the dashboard reads `https://sleekmeeter.app/api/stats?format=json` over the internet instead.

## Reading the numbers

`https://afterclip.app/api/stats` opens the dashboard in the browser: Afterclip and Sleekmeeter together today (UTC) and over the last 7 and 30 days, each app on its own, daily counts split by app, and 30-day breakdowns by Afterclip version, Sleekmeeter version and Voicemeeter edition. It refreshes once a minute while visible. If Sleekmeeter's numbers can't be read, the totals are Afterclip's alone and the page says so.

Each app has its own random ID, so someone who uses both apps is counted once in each.

For scripts:

- `GET /api/stats` (or `?format=json`) returns Afterclip's own totals, in the same shape as Sleekmeeter's.
- `GET /api/stats?format=all` returns `{ total, afterclip, sleekmeeter }`, where `sleekmeeter` is `null` when it can't be read.

Responses can be up to a minute old.

## Test

```bash
node --test server/usage/worker.test.mjs
```

It runs the Worker against an in-memory SQLite database (Node 22.5 or newer) and a stand-in for Sleekmeeter's Worker, so it needs no Cloudflare account.
