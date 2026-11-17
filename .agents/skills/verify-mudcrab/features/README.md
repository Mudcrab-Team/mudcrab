# Mudcrab feature map

Read the feature that matches the change. Record a result for each entry point you drive. Every feature below has source and live coverage with synthetic data. Native input and rendering coverage used Xvfb and NVIDIA Vulkan. Physical desktop behavior and the explicitly unverified entry points still need separate proof.

| Feature | Entry points | Verified coverage and limits |
| --- | --- | --- |
| [Convert and check](convert-and-check.md) | Converter CLI, repair-failed, metadata rebuild; launcher Start, damage confirmation, Check, Full check; Skyrim Data and MO2 profile selection | CLI conversion, cache reuse, metadata rebuild, quick/full checks, corruption rejection, healthy-pack repair refusal. Launcher, MO2, and actual repair remain unverified. |
| [Stop and resume](stop-and-resume.md) | CLI Ctrl+C and resume command; launcher Stop, Resume, Delete staging | CLI SIGINT, retained staging, unchanged old pack, resume, and full check. Launcher controls remain unverified. |
| [Explore and cross doors](explore-and-doors.md) | Engine CLI; launcher PLAY MUDCRAB; native movement, carrying, and E-activated doors; exterior coc and coe commands | Ready engine, native movement, F3, recapture, V, stdin coe, and interior coc rejection. Door crossings, carrying, and PLAY MUDCRAB remain unverified. |
| [Console and settings](console-and-settings.md) | Physical backtick, Escape, V; console commands; settings Save, Resume, and room culling | Native backtick, Tab completion, Help, input suppression, Escape settings, Save, Resume, and Room culling persistence after reload. Radius persistence and room-culling visual effect need larger fixtures. |
| [Reports and captures](reports-and-captures.md) | BugReport local and browser mode; SceneShot; --shots | SceneShot PNG/receipt and settled batch capture. Local report ZIP, hashes, logs, and screenshot verified; report is partial because active-space diagnostics are unavailable. Browser behavior remains unverified. |

Use [the skill](../SKILL.md) for setup, doctor, evidence, and cleanup. The executable proof writes the build and fixture identity, actual commands, independent database queries, output hashes, and cleanup receipt under `reports/verification/<run>/`. Missing prerequisites fail the requested journey. They do not reduce its assertions.
