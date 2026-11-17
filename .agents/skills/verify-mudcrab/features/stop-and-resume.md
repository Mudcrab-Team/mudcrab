# Stop and resume conversion

## Sub-features

- Stop conversion while preserving resumable staging and the previous published pack.
- Resume the recorded staging directory and publish a complete pack.
- Delete abandoned staging without deleting a published pack.

## How to get to it (user POV)

- Press Ctrl+C during a production converter CLI run, then run the exact resume command printed by the converter.
- In the launcher, choose **Stop** during conversion, then **Resume**.
- Choose **Delete staging** from a stopped run with staging available.

## Driving it with the PTY and native desktop

Preconditions:

- Use a separate owned session, healthy doctor result, and a conversion large enough to remain active during interruption.
- Retain hashes of an existing published pack before a replacement conversion begins.
- Use a PTY controller that records the exact child PID identity and sends SIGINT to that process group. Typing the text `^C` is insufficient.

Run `control.py stop-resume --evidence <new-directory>` inside `devenv shell -- python3`, using the script path in the skill. This provides the owned PTY controller, baseline publication, interruption, resume, full check, and cleanup below. Retain `stop.command.json`, `stop.terminal.txt`, `stopped.json`, `resume.command.json`, `resume.terminal.txt`, `resumed.json`, and `stop-resume.json`.

Start a production conversion with a distinct report. During visible work, send one SIGINT to its owned process group. Capture the action and subsequent output. Expect exit 130, JSON `complete: false`, preserved old pack contents, and an existing staging directory. There is no separate stopped-status field. Save the printed resume command and execute it through a new PTY with the converter on PATH. Require a complete manifest, passing integration validation, and successful full check after publication.

For the launcher entry point, open its native window, verify owned Data and Output paths, choose Start, then Stop during active work. Capture the stopped state and staging path. Choose Resume and wait for a complete pack. Repeat in another session to drive Delete staging. Confirm only that staging disappears and the previous published pack's hashes remain unchanged.

Cleanup every failed attempt using the recorded ownership information. Retain interruption, resume, and cleanup transcripts outside scratch.

## Gotchas

- The small generated fixture may finish before interruption. Report that prerequisite failure and obtain a larger disposable fixture. A completed conversion does not prove cancellation.
- The generic `window` helper forwards stdin lines, not terminal signals. Use an actual native button or a controller that sends SIGINT to the recorded child.
- A second Ctrl+C requests immediate exit. It is a different journey from graceful stopping.
- Never guess a staging path or use staging from the user's existing conversion.
- A late Stop can race successful publication. Retry during active work before claiming interruption coverage.
- Metadata rebuild with `--reuse-assets` cannot cooperatively stop or resume. Use normal conversion for this journey.
- Do not interrupt `control.py convert` for this proof. Its failure cleanup removes staging. Use `stop-resume` or a separate controller that retains staging until resume finishes.
