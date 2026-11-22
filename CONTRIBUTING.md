# Contributing to Mudcrab

Thank you for your interest in contributing to **Mudcrab**! We welcome contributions from developers, reverse engineers, 3D graphics enthusiasts, modders, and documentation writers of all experience levels.

---

## 📜 Code of Conduct

Please treat all community members with respect, patience, and empathy. We are all building a modern open-source engine reimplementation together.

---

## 🛠️ How Can You Help?

Here are some areas where you can make an immediate impact:

1. **Asset Converters (`crates/converter`)**
   - `.nif` parser improvements and glTF 2.0 export optimization (`mesh-tools`).
   - `.dds` to KTX2 / Basis Universal texture compression pipeline (`ddsfile`, `basis-universal`).
   - `.esm` binary parsing into SQLite database structures (`nom`, `rusqlite`, `rkyv`).
   - Papyrus (`.pex`) decompilation and transpilation to Luau (`mlua`).

2. **Engine Subsystems (`crates/engine`)**
   - Bevy 0.19+ rendering features (Vercidium instancing, HZB occlusion culling, custom mesh pipelines).
   - Physics integration (rigid bodies, collision meshes, character controllers).
   - Audio spatialization and music state machines.

3. **Launcher & UI (`crates/launcher`)**
   - Bevy UI setup wizard and path detection.
   - Built-in mod manager & load order drag-and-drop workflow.

4. **Documentation & Benchmarks**
   - Refining specs in the [`docs/specs/`](docs/specs/) directory.
   - Writing usage guides, benchmark tests, or API documentation.

---

## 🚀 Getting Started

### 1. Prerequisites
Ensure you have the following installed:
* **Rust** (2024 Edition)
* **Git**
* **A C and C++ compiler** for the bundled SQLite, Luau and Basis Universal sources: Visual Studio
  Build Tools on Windows, Xcode Command Line Tools on macOS, GCC or Clang on Linux

### 2. Fork and Clone
Fork [Mudcrab-Team/mudcrab](https://github.com/Mudcrab-Team/mudcrab) on GitHub, then clone
your fork:
```bash
git clone https://github.com/<your-username>/mudcrab.git
cd mudcrab
```

### 3. Check Workspace Compilation
```bash
cargo check --workspace
```

When Cargo runs inside this workspace, desktop Windows x64 MSVC builds select SQLite's private
heap through `CFLAGS_x86_64_pc_windows_msvc`. This keeps database allocation
separate from decoded records without changing SQL or transaction safety. Other
targets keep their existing allocator. These SQLite-prefixed defines also reach
other C dependencies built for that Windows target. If you supply your own
target-specific CFLAGS, retain `-DSQLITE_WIN32_MALLOC=1 -DSQLITE_WIN32_HEAP_CREATE=1`;
Cargo preserves environment overrides, including
the higher-priority hyphenated target spelling understood by `cc`.

### 4. No Game Installed? Generate Synthetic Assets
```bash
cargo run -p dummy-content -- gen Data
cargo run -p converter --bin converter -- Data modern_assets
```

This produces a deterministic, procedurally built `Data` directory (DDS, PEX, BSA, BA2) with no
copyrighted content, then converts it with the real pipeline. See
[`docs/specs/converters/dummy-content.md`](docs/specs/converters/dummy-content.md) for the CLI,
format coverage and library API.

To look at the generated world in the engine, point it at the converted folder and at the fixture's
worldspace (form id `1`; the engine's default worldspace is Skyrim's Tamriel, which the fixture does
not contain):
```bash
cargo run -p engine --bin engine -- --assets modern_assets --worldspace 1
```

How long a first build and a real conversion take, and how much disk they need, is in
[`docs/specs/meta/requirements.md`](docs/specs/meta/requirements.md).

### 5. Fast Optimised Builds
The engine is slow in a debug build and `--release` spends most of a rebuild on link-time
optimisation. For trying a change, use the `quick` profile (release without LTO, incremental):
```bash
cargo build --profile quick --bin engine
```
The binary lands in `target/quick/`. Use `--release` for anything you measure (benchmarks,
acceptance runs), since `quick` frame times are not comparable.

## 🛠️ Development Workflow

The full path from idea to merge (checking for overlap, claiming an issue, drafts, review, merging) is in [`docs/contributing/workflow.md`](docs/contributing/workflow.md). Who can review, approve and merge, and how people join, is in [`GOVERNANCE.md`](GOVERNANCE.md). The short version:

1. **Find or Create an Issue, and Claim It:**
   Search open issues *and* pull requests, including who is assigned, so no one else is working on the same thing. Assign yourself, or, if GitHub doesn't let you, comment "I'm taking this": the comment counts as your claim.
2. **Create a Feature Branch:**
   ```bash
   git checkout -b feature/nif-skinning-support
   ```
3. **Write Clean, Idiomatic Code:**
   Ensure all functions have doc comments (`///`) and pass clippy checks.
4. **Run What CI Runs:**
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo nextest run --workspace --lib --bins --tests --examples --no-fail-fast --no-tests=warn
   ```
   CI uses [`cargo-nextest`](https://nexte.st) (`cargo install cargo-nextest --locked`); plain
   `cargo test --workspace --lib --bins --tests --examples` runs the same test targets (neither runs doctests; add
   `cargo test --workspace --doc` for those). The Linux `clippy` / `test` jobs run for
   Rust-related changes on PRs and feed `tests_pass`. The same checks run on Windows once a
   night against `main` (`.github/workflows/windows.yml`; start it by hand from the Actions
   tab to check a branch). GPU-only tests stay ignored by default; Linux runs the two software Vulkan
   tests separately. CI also runs `cargo audit` and a release-mode
   performance check; see `.github/workflows/run_tests.yml` for the exact commands.

   Every WGSL file under `crates/engine/src/shaders` and `vendor/` is composed with `naga_oil`
   and validated by Naga in the `engine` tests (`crates/engine/src/shader_validation.rs`). It
   needs no GPU and runs with the normal test suite. Add a variant there when a pipeline uses new
   shader defs; a shader that cannot be composed outside the app goes in its `KNOWN_EXCEPTIONS`
   with a reason.

   CI also counts `.unwrap()`, `.expect(` and `panic!(` in production code (not in `#[cfg(test)]`
   code, and not in `#[test]` functions). It fails when a crate's count is higher in your pull request
   than at its base commit, so it judges only the sites you add and needs no upkeep. Prefer returning an
   error with `?`. To check locally against `main`, add a worktree of it and run
   `python3 tools/unwrap_ratchet/unwrap_ratchet.py --base-root <that worktree>`. If a new call is
   justified, run `python3 tools/unwrap_ratchet/unwrap_ratchet.py --update`, commit the changed
   `tools/unwrap_ratchet/baseline.json` (it also raises `revision`, so the approval shows in the diff even
   when a number does not change), and explain the reason in the pull request. A baseline that your pull
   request did not change never raises the limit.

   CI also counts silent error drops in the same production code. The counted patterns are a
   propagated error whose value is dropped (`let _ = ...?;`), a `Result` error turned into `None`
   (`.ok()`, including `.ok()?`), an error or absence replaced by a default or empty value
   (`.unwrap_or_default()`, `.unwrap_or("")`, `.unwrap_or_else(|_| String::new())`,
   `.unwrap_or_else(|_| String::default())`), failed items dropped from an iterator
   (`filter_map(Result::ok)`), and an error message replaced by an empty string
   (`map_err(|_| "")`, `map_err(|_| String::new())`, `map_err(|_| String::default())`). It fails
   when a crate's count is higher than at its base commit, so it judges only the sites you add.
   To keep an intended drop, put a comment with a reason:
   `// silent-ok: an absent tag means this record has no tag`. Put it on the line of the site, the
   line above it, or the line of the `let` that starts a multi-line statement (or the line above
   that `let`). A marker with no reason is not an exception, and the closing `*/` of a block
   comment is not a reason. Check locally with
   `python3 tools/silent_failure_ratchet/silent_failure_ratchet.py --base-root <a worktree of main>`.
   To accept a justified rise that has no marker, run `--update` (it raises that crate's entry and
   the file's `revision`) and explain the reason in the pull request. The walk ignores
   `#[cfg(test)]` code and test files exactly as the unwrap ratchet does.

   Three limits: a bare `let _ = call();` is not counted, because syntax cannot tell a call that
   returns `Result` from any other call; `.flatten()` is not counted, on `Result<Option<_>>` or on
   `Option<Option<_>>`; and an error replaced by a non-empty invented value is not counted.
   Idiomatic `try_into().ok()` on a fixed-size slice and Bevy's `single().ok()` are counted, because
   `try_into()` can fail on a variable-length slice and `single()` fails when zero or several
   entities match. Keep such a drop with a marker that gives the reason.

   CI also requires a labelled, issue-linked marker on every stand-in for retail data. A stand-in
   is a value or an asset Mudcrab makes where retail ships one. Put
   `STAND-IN(#NNN): <what it replaces>` in the same comment block or Markdown section. An owner
   may be added as `STAND-IN(#NNN, @owner): <what it replaces>`. An added comment that admits a
   stand-in, or an added `PLACEHOLDER`, `GENERATED_*`, `SYNTHETIC`, `DUMMY`, `FAKE` or `LEGACY_`
   identifier, fails without the marker. A malformed marker fails too. Test code is out of scope.
   Existing hits are recorded per file in `scripts/stand_in_baseline.json`; the gate fails when a
   file's count rises. `python3 scripts/check_retail_claims.py --audit` lists them, and
   `--check-issues` reports a closed issue on a schedule. See
   [writing PRs](docs/contributing/writing-prs.md#stand-ins-for-retail-data) and
   [ADR-0015](docs/adr/0015-retail-rule-quirk-defect.md).

   CI also gates four Clippy lints that find lossy numeric casts and exact float comparisons
   (`cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap`, `float_cmp`) in the
   `converter` and `shared` crates. The gate fails only when one of them fires on a line your pull
   request adds; an existing site on unchanged lines is reported as one total and never fails. A
   diagnostic counts when any line of its span is added. A macro site counts when any line of its
   definition or of a call site is added, at every nesting level. The gate reads the JSON log of the
   normal Clippy pass, so it adds no second build. It runs on Linux, so a cast under
   `#[cfg(windows)]` is not checked.

   A moved or renamed line is not new: an added line whose exact text is also deleted in the same
   diff is skipped, even when the deletion is in a wholly deleted file or another crate. The gate
   therefore proves "no net-new identical cast line", not "no new cast"; do not read a green gate
   as proof that a change adds no cast.

   The gate covers the lib and bin targets, because `--all-targets` compiles their test harnesses
   too, and the log must contain those test builds; integration tests, benches and examples are
   excluded. The pass does not use `-D warnings`, because the later `-W` makes the four lints
   warnings and the deny then turns them into errors; the gate fails on every other warning
   instead. An exception needs a reason at the cast site, for example
   `#[allow(clippy::cast_possible_truncation, reason = "the form ID is below 2^24")]`.
   No lint checks the reason, so an allow without `reason =` is a review finding.

   To check locally, run the two commands separately:
   `python3 tools/cast_ratchet/cast_ratchet.py --lint-flags` prints the Clippy flags, and
   `python3 tools/cast_ratchet/cast_ratchet.py --clippy-json <log> --base origin/main` runs the
   gate. `--lint-flags` prints the flags and runs nothing, so it rejects `--base` and
   `--clippy-json`.

   The byte-level parsers (ESM subrecords and record framing) also have `cargo-fuzz`
   targets in `fuzz/`. They run on a schedule, not on pull requests.
   [`docs/contributing/fuzzing.md`](docs/contributing/fuzzing.md) shows how to run one
   for a fixed time and what to do with a crash.

   The record readers also have a scheduled `cargo-mutants` run that lists tests which pass
   without testing a change. [`docs/contributing/mutants.md`](docs/contributing/mutants.md)
   explains the report and how to run one locally.

   Every open issue and pull request also needs a milestone. A pull request inherits the
   milestone of the issues it links when they agree; anything else gets the `needs milestone`
   label, and the daily check fails while an open item has none. Contributors cannot set a
   milestone, so a maintainer assigns one within one working day. See
   [workflow.md](docs/contributing/workflow.md#milestones).
5. **Submit a Pull Request:**
   - Target the `main` branch and fill in the PR template; [`docs/contributing/writing-prs.md`](docs/contributing/writing-prs.md) shows how to write it so it is quick to review.
   - Link the issue (`Closes #N`), apply the [labels](docs/contributing/labels.md) that fit (if a label there doesn't exist on the repository yet, use the closest one that does; without triage access, name them in the PR instead), and reference any relevant planning docs in `docs/specs/`.
   - Used an AI assistant? Follow [`docs/AI_POLICY.md`](docs/AI_POLICY.md): disclose it in one line, and own every line you submit.

---

## ⚖️ License & Legal

By contributing to Mudcrab, you agree that your contributions will be licensed under the **GNU General Public License v3 or later** (`GPL-3.0-or-later`).

### Legal Disclaimer
Mudcrab is a clean-room engine reimplementation. **Do NOT upload, distribute, or submit copyrighted game assets** (`.bsa`, `.esm`, `.nif`, `.dds`, etc.) owned by Bethesda Softworks / ZeniMax Media in PRs or issues. All test fixtures must be generated procedurally or extracted dynamically at runtime from the user's legally owned game files.
