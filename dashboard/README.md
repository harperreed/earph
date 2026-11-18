# Earph live progress

The private dashboard lives at <https://artifacts.folk-fujita.ts.net/p/earph-progress/>.
The machine's Tailscale identity authenticates publication.
No API key is needed.
The [artifact host skill](https://artifacts.folk-fujita.ts.net/skill.md) defines the publishing and live-data contracts.

Edit `dashboard/status.json` after verified work changes.
Record the verification boundary and the remaining qualification limits.
Label inferred causes.
If a trace omits a state value, do not present that state as observed.
Group verified entries by capability.
Use the activity list for recent events.
Update `updated_at` only when those facts are verified.
Keep private media, device identifiers, account details, and machine paths out of dashboard facts.

The builder derives package names, dependencies, and milestone definitions from `docs/SPEC.md`.
The requirement catalog remains authoritative in `docs/requirements.json`.
The dashboard shows scoped package status, not a release completion percentage.

Run `scripts/check-progress` for model, builder, and syntax checks.
Run `scripts/publish-progress` after changing page code or publishing a saved snapshot.
Run `scripts/publish-progress --data-only` for facts delivered to open pages through the live store.
The publisher verifies ownership and private visibility before each update.
It reads remote state again after each write.
It retains operation receipts under ignored `target/progress-dashboard/receipts/`.
If an operation stops, inspect its receipts before retrying.

The page loads its own files through relative paths.
It uses the host helper's `data.watch` for `progress/current` updates.
It uses no polling, browser storage, or direct API calls.
It retains the last valid snapshot if a malformed update arrives.
A live connection and old evidence are separate states.
The page marks evidence older than 30 minutes as stale.

Run `scripts/check-progress --browser` for the real published desktop and mobile checks.
Playwright must exist at `target/progress-browser/node_modules/playwright`.
Set `PROGRESS_CHROMIUM` to an existing Chromium executable if the task's default is unavailable.
The browser checks preserve TLS verification.
They retain screenshots and logs under ignored `target/progress-dashboard/browser/`.
Run `scripts/check-progress --publish-verification` to record successful browser checks and verify a real live update.
This command updates `status.json` with the observed result and publishes that fact.
Browser installation is separate from verification.
If installing browsers, set `PLAYWRIGHT_SKIP_BROWSER_GC=1` and an isolated `PLAYWRIGHT_BROWSERS_PATH` under `target/`.
