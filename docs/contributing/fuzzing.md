# Fuzzing the byte-level parsers

The `fuzz/` directory is a separate [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)
workspace. It is not a member of the root workspace, so ordinary builds, Clippy and
tests keep the pinned stable toolchain and never need nightly.

Each target reads untrusted bytes and asserts one contract: no panic, no abort, and no
unbounded allocation. `Ok` and `Err` both pass.

| Target | Entry point | What it covers |
| --- | --- | --- |
| `esm_subrecords` | `converter::esm::extractors::extract_subrecords` | Subrecord framing, including `XXXX` extended lengths. |
| `record_inflate` | `converter::records::fuzz_inflate` | One compressed record payload and its 64 MiB ceiling. |
| `record_scan` | `converter::records::fuzz_scan` | Record framing, group nesting, inflation and recovery. |

The two non-public entry points are re-exported from `crates/converter/src/records`
only when the crate is built with `--cfg fuzzing`, which is what `cargo fuzz` sets.

## Run locally

1. Install nightly Rust and cargo-fuzz:

   ```sh
   rustup toolchain install nightly
   cargo install cargo-fuzz --locked
   ```

2. Copy a committed seed directory to a writable location. libFuzzer adds inputs to
   the corpus directory it is given, so do not point it at the checkout:

   ```sh
   mkdir -p /tmp/mudcrab-fuzz/corpus
   cp -R fuzz/seeds/esm_subrecords /tmp/mudcrab-fuzz/corpus/
   ```

3. Run one target. The flag after `--` is a libFuzzer argument:

   ```sh
   cd fuzz
   CARGO_TARGET_DIR=/tmp/mudcrab-fuzz/target cargo +nightly fuzz run \
     esm_subrecords /tmp/mudcrab-fuzz/corpus/esm_subrecords -- \
     -max_total_time=60 -rss_limit_mb=2048 -timeout=10 -max_len=65536
   ```

   Replace `esm_subrecords` with `record_inflate` or `record_scan`. In this repository,
   point `CARGO_TARGET_DIR` outside the worktree, as
   [storage.md](storage.md) requires; `/tmp/mudcrab-fuzz` is an example.

4. A crash writes an input and a stack trace under `fuzz/artifacts/<target>/`. Run it
   again with that file as the corpus to reproduce:

   ```sh
   cd fuzz
   CARGO_TARGET_DIR=/tmp/mudcrab-fuzz/target cargo +nightly fuzz run \
     esm_subrecords artifacts/esm_subrecords/crash-<hash>
   ```

   Use `cargo fuzz tmin` to minimise it. Add a normal regression test for the failing
   parser before fixing it. Never commit an input derived from a game file; the seeds
   and any committed crash input stay synthetic.

5. `python3 fuzz/generate_seeds.py` rewrites `fuzz/seeds/` from the converter's unit-test
   fixtures. Run it only when those fixtures change, and review the diff.

## Scheduled run

`.github/workflows/fuzz.yml` runs every target for five minutes on Monday each week, and
on demand from the Actions tab with a chosen duration. Pull requests do not start it.
Crashes are uploaded as the `fuzz-crashes-<run id>` artifact and kept for 90 days.
