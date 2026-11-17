# Convert and check an asset pack

## Sub-features

- Convert a fresh Skyrim Data directory into runtime assets.
- Select loose Data or a Mod Organizer 2 instance and profile in the launcher.
- Check file existence and sizes, then verify hashes with Full check.
- Convert again and observe actual cache reuse without weakening pack validation.
- Confirm or cancel conversion over damaged output.
- Preview and apply targeted asset repairs, or rebuild metadata from verified package assets.

## How to get to it (user POV)

- Run `converter <Data> <output> --report-json <report>` in a terminal.
- Open the launcher, choose **Skyrim Data**, drop existing Data and Output directories into the window, and choose **Start**.
- Choose the launcher MO2 source and select its instance and profile before **Start**.
- Choose **Check** or **Full check**, or run `converter check <output> [--full]`.
- After successful conversion, the launcher offers **Convert again**.

## Driving it with the PTY helper and native desktop

Preconditions:

- Run the skill's build command. Use a new evidence directory and a healthy doctor result.
- The seed contains a deterministic plugin, mesh, textures, scripts, an interior, and two linked doors.
- For MO2, supply a disposable instance with a real profile and competing assets. The seed does not provide one.

Run `control.py prove --evidence <new-directory>` inside `devenv shell -- python3`, using the script path in the skill. This runs the production generator, converter with CPU encoders, quick check, and full check through separate PTYs. It removes scratch after recording evidence. The healthy-pack commands exit zero. The report and manifest are complete, integration validation passes, and the read-only database observations confirm both door links and the generated cells. Both checks print `All good:` and preserve pack contents. The proof then flips one byte in a disposable copy. Quick check still exits zero, full check exits one, and both packs retain their contents through checking. Retain the negative-check transcript and hashes.

For launcher conversion, use `prepare` from the skill. Read the owned scratch path from `session.json` and create an empty `modern_assets` directory there. Then use `window --binary launcher`. Drop the existing Data and output folders into the window. The launcher classifies folders by contents, regardless of the field under the pointer. Capture the displayed paths before choosing Start. Wait for completion, inspect the published pack independently, then drive Check and Full check through their actual buttons. Capture visible results and files. The CLI proof does not cover these buttons.

For cache reuse, retain a prepared session, convert once, then run the same production CLI with a distinct report and transcript. Require observed cache hits and a complete pack, then run full check again. The fresh-conversion helper deliberately refuses an existing output. Do not call the cache journey passed without driving this second conversion.

For damaged-output confirmation, use a disposable pack copy with malformed manifest or integration-report JSON. Capture file hashes before choosing Start, Start over, Convert again, or Resume as offered. Require the damage explanation and a matching **Confirm** button. Choose **Cancel** first and require unchanged pack contents. Repeat and confirm after the 500 ms accidental-click delay. Require conversion to start only after confirmation. A running game disables publishing actions.

For targeted repair, require a fixture whose manifest records an actual repairable failure. Run `converter repair-failed <Data> <pack> --texture-encoder cpu --lod-encoder cpu` with the original reader and schema identity. Compare pack contents before and after preview. Preview retains a repair directory and can recover an interrupted repair publication. Repeat with `--apply` and inspect the repaired manifest and full check. Asset repair cannot resolve record loss. A healthy pack rejects repair because it has no failed inputs.

For metadata rebuild, use a separate owned output and run `converter <Data> <new-output> --reuse-assets <verified-pack> --texture-encoder cpu --lod-encoder cpu --report-json <distinct-report>`. Inspect completion and run full check. Retained models, textures, and scripts come from the verified pack. Use normal conversion after those source assets change.

## Gotchas

- The minimal seed contains loose inputs. Archive ingestion, MO2 conflicts, GPU encoding, and retail compatibility need separate fixtures.
- Exit zero does not imply a complete pack. Inspect all three completion reports.
- Quick check checks sizes. It can miss corruption that preserves file length.
- The launcher discovers installed Skyrim through the real environment. Verify its displayed paths before Start.
- A reused evidence path is refused. Keep failed evidence and create a new run.
