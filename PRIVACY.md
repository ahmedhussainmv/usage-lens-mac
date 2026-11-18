# Privacy

Usage Lens is local-first.

**Read:** `.jsonl` history files under `~/.codex/sessions`, `~/.codex/archived_sessions` and `~/.claude/projects`. They may contain private conversation text; the app does not store it.

**Stored locally** in `~/Library/Application Support/Usage Lens Mac/usage.sqlite` (private permissions): token counters, model and effort names, hashed session identifiers, project folder names, parsing cursors, your rates, your Work/Personal choices and a few settings. No prompts, responses, file contents or API credentials.

**Network:** none, except an update check that you start (or opt in to). It runs the system `curl` once against the project's download page, over HTTPS only, and sends the app version in the User-Agent header. Nothing else leaves your Mac. There is no analytics, crash reporting or account.

**Feedback** opens a pre-filled GitHub issue in your browser containing the app and macOS versions. Nothing is sent until you submit it, and you can edit it first.

**Export CSV** writes a file only where you choose. It contains project folder names and token counts.

**Demo mode** (`--demo`) uses generated data in a temporary folder and reads nothing from your Mac.
