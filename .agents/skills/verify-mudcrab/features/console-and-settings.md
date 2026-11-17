# Use the console and save settings

## Sub-features

- Open and close the console without leaking gameplay input.
- Run Help, completion, and registered commands.
- Open settings, change a value, save, and reload it.
- Resume gameplay with cursor ownership and pause state restored.
- Toggle interior room culling on the Graphics page.

## How to get to it (user POV)

- Press the physical backtick key to toggle the engine console.
- Run `Help`, `ClearConsole`, `tcl`, or `tcg` in the console.
- Press Escape to open settings, change **Full-detail radius (cells)** on the Streaming page, and choose **Save**.
- Choose **Resume** to close settings.
- The CLI option `--stdin-console` provides a separate developer command entry point.

## Driving it with the native desktop and stdin console

Preconditions:

- Open a prepared engine session through `window` and wait for a usable rendered scene.
- Use a native driver that records physical key presses and window screenshots.
- Keep the helper's isolated XDG configuration directory for a second engine launch.
- To save Full-detail radius, supply a fixture with every cell in the selected radius and launch without a `--stream-radius` override. The one-cell helper fixes this option at zero, and Save preserves launch-owned settings. Use Room culling for persistence with the minimal seed.

Capture the initial scene and player position. Click to capture the cursor, then press physical backtick. Type `Cle`, press Tab, and capture the completed `ClearConsole` input before Enter. Type `Help` and verify command output. Type `tcl` and observe the reported mode. While the console is open, send movement input and verify the player does not move. Close with backtick and verify movement and cursor capture resume. Separately close with Escape, require the cursor to remain released, and click to recapture it.

Press Escape and record the setting under test. With the minimal seed, change Room culling on the Graphics page. With the larger fixture and no radius override, change Full-detail radius on the Streaming page. Choose Save and capture the saved status. Inspect `<scratch>/config/Mudcrab/settings.json`. Choose Resume and confirm gameplay resumes. Close the engine normally, launch it again in the same prepared session, and reopen settings. Require the saved value to remain visible and agree with the file. Retain both launch transcripts and screenshots before cleanup.

For the stdin entry point, send `Help` or `tcl` as a line to the running helper. Capture the command input log and resulting engine output. Mark native console opening and focus as unverified unless physical keys were exercised.

On the Graphics page, find **Room culling** under **Culling and drawing**. It defaults on. Toggle it, Save, and verify the value after reopening the app. A visible interior with converted room data is required to verify the culling effect. The minimal seed has no room graph, so a saved toggle alone does not prove the visual effect.

## Gotchas

- A literal backtick character in stdin is not a physical keyboard event.
- Sending commands through stdin bypasses console opening and cannot prove overlay focus or pause behavior.
- The helper isolates settings through XDG_CONFIG_HOME on Linux. Do not point this run at the user's configuration directory.
- Saving the file alone is insufficient. Reopen the app and inspect the value.
