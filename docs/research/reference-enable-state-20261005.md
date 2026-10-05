# Reference enable-state evidence for #165

Date: 2026-10-05. This note records the documentary rule and its limits before spawn filtering. It does not claim retail runtime parity.

## Skyrim Creation Kit rule

The Skyrim Creation Kit [Reference page, revision 24591](https://ck.uesp.net/w/index.php?title=Reference&oldid=24591#Enable_Parent), updated 2026-03-28, describes an enable parent as the authority for a child reference's state. It says:

> Their disable/enable state is always determined by their enable parent.

The same section says the child follows the parent's enabled state, or follows its inverse when “Set Enable State to Opposite of Parent” is checked. This means a child's `Initially Disabled` header bit (`0x800`) does not override an existing enable parent. That conclusion follows from the Creation Kit documentation; it is not a measurement of Skyrim's runtime.

The page's wikitext was read from this MediaWiki API request:

`https://ck.uesp.net/w/api.php?action=query&prop=revisions&titles=Reference&rvprop=ids%7Ctimestamp%7Ccontent&rvslots=main&format=json`

Pinned revision: page ID `4900`, revision `24591`, timestamp `2026-03-28T21:44:04Z`. SHA-256 of the UTF-8 revision wikitext: `caa89afaecfc72102b1ad8d34532d343001c2a4660fd8c5acfd2d3429edafe21`. The wiki page returned HTTP 403 during this check; the revision API returned the content used for this hash.

## FormID `0x14` and the new-game assumption

The local capture of PR #136's validation report is [/home/dev/Projects/mudcrab-skyrim-reference/research/skyrim-reference/mapping/targets.json:639](/home/dev/Projects/mudcrab-skyrim-reference/research/skyrim-reference/mapping/targets.json:639). Its captured PR body reports a read-only check over 80 official plugins and says all 16 remaining unresolved XESP parent IDs were `00000014`, the hardcoded player reference absent from plugin records. The capture identifies PR #136 at lines 636–642; the live PR is [#136](https://github.com/Mudcrab-Team/mudcrab/pull/136). This is a report of converter/database validation, not an independent runtime observation, and the local `targets.json` capture is currently untracked in the reference worktree.

The Skyrim Creation Kit [GetPlayer page, revision 25272](https://ck.uesp.net/w/index.php?title=GetPlayer_-_Game&oldid=25272) defines the return value as “The Actor that represents the player.” Its Notes identify the auto-filled `PlayerRef` property as hardcoded `ACHR:00000014`. The pinned API response is `https://ck.uesp.net/w/api.php?action=query&prop=revisions&titles=GetPlayer%20-%20Game&rvprop=ids%7Ctimestamp%7Ccontent&rvslots=main&format=json` (page ID `3702`, revision `25272`, timestamp `2026-09-06T01:02:44Z`); its UTF-8 wikitext SHA-256 is `e34dd850ab7a01de137da5c889667c56d582fab0f959b440bb3abafe551180b2`.

These sources establish the identity and role of `0x14`, but do not state that it is always enabled in a clean new game. Treating PlayerRef as enabled for a new-game static snapshot is an explicit implementation assumption based on the player's required role, not a source-verified engine fact. A clean-save runtime check is still needed before claiming retail parity.

## XESP bytes and deleted references

The pinned [TES5Edit Skyrim definitions](https://github.com/TES5Edit/TES5Edit/blob/9fb016884bec138ea6c7b872cec831537d464c3e/Core/wbDefinitionsTES5.pas#L3075-L3082) define XESP as a parent FormID, a one-byte flags field, and three unused bytes. The flags are ordered as `Set Enable State to Opposite of Parent` (bit 0) and `Pop In` (bit 1). The local format review records this layout at `research/skyrim-reference/mapping/workers/plugin-comparators.md:13`.

Mudcrab's exporter reads bytes 4–7 of XESP into a full little-endian `u32` at [exporter.rs:698](/home/dev/.t3/worktrees/mudcrab/issue-165-parent-evidence/crates/converter/src/esm/exporter.rs:698). That stored word includes the named flags byte and the three unused bytes. Spawn-state evaluation should inspect bit 0 only; bit 1 controls pop-in and the upper 24 bits are reserved payload, not additional enable-state flags. The converter's layout comment is at [mod.rs:365](/home/dev/.t3/worktrees/mudcrab/issue-165-parent-evidence/crates/converter/src/esm/mod.rs:365).

The converter recognizes deleted records by header bit `0x20` at [records/mod.rs:19](/home/dev/.t3/worktrees/mudcrab/issue-165-parent-evidence/crates/converter/src/esm/records/mod.rs:19), and removes a deleted winning FormID during plugin merge at [mod.rs:122](/home/dev/.t3/worktrees/mudcrab/issue-165-parent-evidence/crates/converter/src/esm/mod.rs:122). Deleted winning references therefore do not reach the final reference export. PR #136 also reports that invalid optional parent links are normalized to parent ID zero; zero is the no-parent case, while the original flags are preserved (captured body at `targets.json:639`).

## Unresolved cases and evidence boundary

The Creation Kit documentation and the cited converter report do not establish Skyrim's initial behavior for a nonzero parent missing from the effective database, a deleted parent, or an XESP cycle. Their state policy remains unresolved as a question of Skyrim parity. Mudcrab may omit such references and report them as a conservative policy, but that would be an implementation choice rather than documented game behavior. The same applies to any fallback for malformed XESP metadata.

No Skyrim game process or clean-save observation was available during this review. The source-backed rule is parent inheritance with optional bit-0 inversion; dynamic save state, scripts, missing/cyclic links, and the `0x14` initial enabled state remain outside the evidence.
