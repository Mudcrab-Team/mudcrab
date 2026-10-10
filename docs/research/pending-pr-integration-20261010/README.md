> Written with an AI assistant (Codex).

# Pending PR integration — 2026-10-10

The [integration branch](https://github.com/Mudcrab-Team/mudcrab/tree/t3/merge-pending-prs) combines the captured open PRs so their interactions can be reviewed together. The current [inventory](inventory.json) contains **31 PRs, including 12 drafts**. PR [#213](https://github.com/Mudcrab-Team/mudcrab/pull/213) appeared during the work and was added to the scope. Draft status remains a review signal; inclusion here does not approve or land a PR on its target branch.

**The complete 31-PR integration passes the recorded local checks.** The final recovery repair is included in the validated checkpoint below. Retail, native GPU and performance gates remain open.

| Final checkpoint | Value |
| --- | --- |
| Validated code head | `864cda965740dbd19f252b76a7441b93698bf49c` |
| Captured heads verified reachable | `31` / 31 |
| Rust results | `1,869 passed; 28 ignored; 86 targets` |
| Python results | `301 passed` |
| Build, Clippy, formatting and tooling gates | `Passed: workspace all-target check, strict all-target/all-feature Clippy, formatting, schema generation, workflow and spec checks` |

The [player changelog](CHANGELOG.md) lists what to try. Follow-up [runtime repair evidence](RUNTIME-REPAIRS.md) covers the missing door projection, authored clutter pickup, action prompts and measured LOD selector work. The checkpoint below records the earlier integration validation; follow-up results are recorded separately.

## Scope and evidence

The inventory records each PR's URL, exact `headRefOid`, base branch, draft status, changed files and captured ancestor PRs. The scope is #122, #123, #124, #125, #126, #128, #154, #161, #163, #167, #171, #173, #174, #175, #189, #190, #191, #197, #198, #200, #201, #204, #205, #206, #207, #208, #209, #210, #211, #212 and #213.

| Evidence | What it establishes |
| --- | --- |
| [Live refresh](live-refresh.json) | Origin and GitHub pending heads, bases, draft flags and titles match the captured snapshot. |
| [Inventory](inventory.json) | Captured PR identities and exact heads; later upstream changes require a fresh capture. |
| [Integration audit](integration-audit.json) | Captured ref/head equality, head ancestry in the audited integration commit, file-inventory consistency and CI base coverage. |
| [Merge ledger](merge-ledger.json) | Integration checkpoints and whether a head was merged directly or already contained through another merge. |
| [Recorded conflicts](recorded-conflicts.json) | Files requiring manual resolution during the recorded merges. Clean merges can still require semantic repair. |
| [Validation](validation.json) | Host, toolchain, per-target results, check outcomes and local log paths with SHA-256 digests. Log contents are not embedded in this directory. |

The captured refs use `refs/integration/20261010-366adc03/pr-N`. Reachability proves that an exact PR head is in the integration history. It does not prove that subsequent conflict resolutions preserve every behavior; the repair cases below cover concrete interactions found during integration.

## Repairs

The published repair checkpoints are [`01fb67f`](https://github.com/Mudcrab-Team/mudcrab/commit/01fb67f), [`62af074`](https://github.com/Mudcrab-Team/mudcrab/commit/62af074) and [`e55a12a`](https://github.com/Mudcrab-Team/mudcrab/commit/e55a12a). The [#213 merge](https://github.com/Mudcrab-Team/mudcrab/commit/f3a6a5c), [warm durability/copy repair](https://github.com/Mudcrab-Team/mudcrab/commit/927f07f), and [manual CI repair](https://github.com/Mudcrab-Team/mudcrab/commit/864cda9) are included in the final checks.

| Area / PR interaction | Concrete failure or risk | Repair and validation boundary |
| --- | --- | --- |
| CI / stacked PR bases | The branch allowlist omitted seven captured stacked bases. Moving-base comparisons could hide required checks. | Removed the PR base allowlist; use full history and the immutable PR-event base. All 11 jobs participate in aggregation. Offline Git fixtures cover audit and change-filter behavior. Manual dispatch checks every tracked owning path after evidence-only commits; producer-free matching avoids SIGPIPE misclassification with large path inventories. |
| Tooling / #161, #163, #207 | Rust contract changes could bypass schema checks; source anchors and capture wrappers described older contracts. Native observer changes lacked macOS CI coverage. | Expanded owning-source filters, refreshed evidence and unique text anchors, and require current producer/world contracts in capture preflight. Native observer configuration tests run on Linux/macOS; these are not renderer captures. |
| Converter / #126, #198, #204, #208, #209 | Independent schema/version bumps could reuse incompatible outputs. Localization could be resolved twice or omitted from database cache identity. | Combined producer **27 / world DB 9**; distinguish normal rebuilds from explicit metadata retention. Preserve reader and selected-language identities, stage winning string banks before both readers/cache lookup, and export already resolved in-house text once. Fixtures cover cache misses, winner precedence and retained provenance. |
| Converter / #154, #190, #198 | Cold MO2 output and differently cased string directories exposed discovery and localization gaps. | Guard optional output directories, retain canonical archived banks, and resolve winning loose banks at the owning source boundary. The separate Linux directory-alias gate remains unrun. |
| Converter / #208, #209, #213 | Pack recovery, deduplicated blobs, coverage recipes and durability policies must agree before reuse. | #213 reconciles typed selection with recipes, verified sealed-pack recovery, v1/v2 compatibility and atomic blob repair. New CLI/config runs default to **Archive**; legacy serialized configs retain **PerFile** defaults. Explicit policies remain available. See the [cache integration contract](../converter-cache-integration-20261010.md). Warm restore preserves per-hash ownership, flushes physical payloads after None→PerFile, and measures streamed fallback copies; an 18-case regression passes. Counters exclude read/hash/metadata I/O. |
| Engine / #124, #128, #210, #211 | Console movement, priorities and pacing could use startup worldspace or stale exterior coordinates after crossing a door. | Use current `ActiveSpace` identity/position, reject console transitions during a pending crossing, clear interior state on exit, and reset controller history on identity changes. Headless regressions cover interior and same-grid worldspace transitions. |
| Engine / #125, #128, #205, #211 | Dynamic clutter could escape collision reservations; controller writes could override landing upload relief; native material images could appear ready too early. | Reserve dynamic collision costs, preserve unlimited landing uploads across controller updates and restore the latest decision. Explicitly inspect native material base image dependencies and retain reservations when a typed store is absent. Regression tests cover repeated landing updates and missing image dependencies. |
| Engine / #200, #211 | Generated terrain batches retain source and replacement resources that the admission ledger does not account for. | Disable batching when request/admission controls are enabled, with an actionable message; independent opt-in modes remain usable. Generated retained-byte accounting is a prerequisite for enabling their combination. Default Bevy upload budgeting alone keeps batching available. |
| Engine / #122, #206, #212 | Route/comparison benchmarks could miss pacing instrumentation or compete with benchmark camera jumps. | Install timing through the shared benchmark predicate, give routes camera ownership, and preserve frame/trace serializers plus native window/build metadata. Configuration and route regressions cover the combined options. |

An [interactive snapshot](pending-state.html) provides PR search, draft/base filters and hotspot details. Remediation comment receipts are recorded in [published comments](published-comments.json).

## Published remediation comments

All 14 comments identify the captured PR head, link the integration repairs and keep fixture results separate from native/retail acceptance. Their exact bodies and authors are in the [receipt file](published-comments.json).

| PR comment | Publication check |
| --- | --- |
| [#124](https://github.com/Mudcrab-Team/mudcrab/pull/124#issuecomment-6097438576) | Published and exact body verified through GitHub readback |
| [#128](https://github.com/Mudcrab-Team/mudcrab/pull/128#issuecomment-6097438975) | Published and exact body verified through GitHub readback |
| [#154](https://github.com/Mudcrab-Team/mudcrab/pull/154#issuecomment-6097439376) | Published and exact body verified through GitHub readback |
| [#163](https://github.com/Mudcrab-Team/mudcrab/pull/163#issuecomment-6097439710) | Published and exact body verified through GitHub readback |
| [#167](https://github.com/Mudcrab-Team/mudcrab/pull/167#issuecomment-6097440034) | Published and exact body verified through GitHub readback |
| [#198](https://github.com/Mudcrab-Team/mudcrab/pull/198#issuecomment-6097440351) | Published and exact body verified through GitHub readback |
| [#200](https://github.com/Mudcrab-Team/mudcrab/pull/200#issuecomment-6097440725) | Published and exact body verified through GitHub readback |
| [#204](https://github.com/Mudcrab-Team/mudcrab/pull/204#issuecomment-6097441075) | Published and exact body verified through GitHub readback |
| [#207](https://github.com/Mudcrab-Team/mudcrab/pull/207#issuecomment-6097441416) | Published and exact body verified through GitHub readback |
| [#208](https://github.com/Mudcrab-Team/mudcrab/pull/208#issuecomment-6097441767) | Published and exact body verified through GitHub readback |
| [#209](https://github.com/Mudcrab-Team/mudcrab/pull/209#issuecomment-6097442186) | Published and exact body verified through GitHub readback |
| [#211](https://github.com/Mudcrab-Team/mudcrab/pull/211#issuecomment-6097442584) | Published and exact body verified through GitHub readback |
| [#212](https://github.com/Mudcrab-Team/mudcrab/pull/212#issuecomment-6097442911) | Published and exact body verified through GitHub readback |
| [#213](https://github.com/Mudcrab-Team/mudcrab/pull/213#issuecomment-6097443633) | Published and exact body verified through GitHub readback |

## Future merge risks

The audit distinguishes independent changes from inherited stack overlap: an ancestor/descendant PR pair is excluded from the independent count. At the validated checkpoint, 53 paths had independent overlap among 177 shared paths. The hottest paths were:

| Path | Independent PR pairs | Keep this contract explicit |
| --- | ---: | --- |
| `crates/engine/src/config.rs` | 76 | One parser and shared enable predicates; script options must match it. |
| `crates/engine/src/app.rs` | 56 | System ordering, camera ownership and benchmark instrumentation. |
| `crates/engine/src/streaming.rs` | 51 | Active-space identity, resource lifetime and collision/admission accounting. |
| `SPEC.md` | 42 | Unique requirement IDs and valid references. |
| `crates/converter/src/pipeline.rs` | 39 | Selection, localization, recovery and publication order. |
| `crates/engine/src/lib.rs` | 38 | Registration must retain every required module and plugin. |
| `.github/workflows/run_tests.yml` | 18 | All stacked bases, dispatches and owning paths receive their required gates. |
| `crates/converter/src/metadata.rs` | 17 | Producer/schema identity and explicit retention compatibility. |

These counts rank review attention, not defect probability. For later work in these paths, declare the shared contract in the PR, keep compatibility rules in one owning layer, and add an interaction regression when behavior crosses another feature's boundary. Preserve historical benchmark evidence with its original commit and configuration.

## Validation and open gates

On `aarch64-apple-darwin`, Rust 1.98.1 and Python 3.11, checkpoint `864cda965740dbd19f252b76a7441b93698bf49c` passed **1,869 Rust tests across 86 targets**, with **28 ignored**, and **301 Python tests**. The complete final converter-library recheck replaces its earlier result. Target identity includes the binary/crate and source path; repeated runs are not counted twice. The engine library contributed 724 passing tests and one ignored test. Native observer tests are included within the Python total.

Workspace all-target check, all-target/all-feature Clippy with warnings denied, formatting, schema generation and workflow checks passed. The spec audit found 366 unique V/T/B definitions and zero undefined references. These results establish the integrated code and fixture behavior.

| Unrun gate | Evidence still needed |
| --- | --- |
| Retail conversion | Convert real installed inputs with the final combined producer; validate manifests, logical databases, terrain proofs and assets. |
| Native GPU / visual behavior | Final renderer captures and installed-game inspection on supported native platforms; ignored GPU and external-oracle tests remain separate. |
| Performance | Repeated matched cold/warm runs of the final binary, with pinned inputs, cache state, worker counts and output validation. Earlier PR benchmarks do not measure this integration. |
| Linux casing | Run the case-sensitive string-directory alias fixture on Linux. |
| Platform CI | Run the native Windows/Linux matrix, including platform-specific flush/link behavior. |
| Schema explorer | Exercise the current browser interaction; generated output and unit tests cover different behavior. |

Fixture recovery tests do not establish actual power-loss recovery. Human/code-owner review and the draft PRs' acceptance gates remain required before landing their changes.

To refresh ancestry and overlap evidence against a fixed final code head, run from the repository root:

```sh
python3 scripts/audit-pr-integration.py \
  --inventory docs/research/pending-pr-integration-20261010/inventory.json \
  --ref-prefix refs/integration/20261010-366adc03 \
  --integration-head <final-code-head> \
  --output docs/research/pending-pr-integration-20261010/integration-audit.json
```
