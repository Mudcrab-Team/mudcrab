# Self-hosted CI runner (tuvalu-laptop)

A cached Linux runner for `Mudcrab-Team/mudcrab`, run in a distrobox on a NixOS host.
Host tools (`distrobox`, `gh`, `rsync`) come from the devenv `ci-runner` profile, so no
system rebuild is needed.

## Layout on the host (`$CI_ROOT`, default `~/ci/mudcrab`)

| Path | Contents | Bound |
| --- | --- | --- |
| `control/` | checkout of trusted `main` that the service runs from | |
| `home/` | the box's private `$HOME` | |
| `cache/cargo`, `cache/sccache`, `cache/target` | one shared copy each | `CI_TARGET_MAX_GB` (60) clears `target`; sccache is capped at 30G |
| `cache/store/assets/schema-N` | converted Skyrim assets, one pack per converter schema | newest `MUDCRAB_KEEP_SCHEMAS` (2) packs |
| `cache/skyrim/Data` | Skyrim source, mounted read-only | |

A job at a schema that already has a complete pack reuses it. A new schema starts from a
reflink copy of the newest older pack, so the converter upgrades it in place and no
second full copy is written.

## Commands

```sh
devenv --profile ci-runner shell -- ci/self-hosted/runner image    # build image
devenv --profile ci-runner shell -- ci/self-hosted/runner create   # create the box
devenv --profile ci-runner shell -- ci/self-hosted/runner run      # serve jobs
ci/self-hosted/sync-skyrim <Skyrim Data dir>                       # from the machine with Skyrim
```

`mudcrab-ci-runner.service` is a systemd user unit for `run`. Install it into
`~/.config/systemd/user/`.

## Trust

Each job gets a single-use JIT registration, and only trusted events reach the runner:
`run_tests.yml` sends fork pull requests to `ubuntu-latest`. The box has its own home and
no `gh` credential, but distrobox is not a security boundary: processes run as the host
user and the host filesystem is reachable under `/run/host`. Do not route untrusted code
to it.

This directory is mounted read-only at `/opt/ci-tools` in the box (a directory mount, so
updates to the control checkout are seen without recreating the box). In a job,
`/opt/ci-tools/artifact-store ensure-assets` converts (or reuses) the asset pack and writes
`assets=<path>` to `$GITHUB_OUTPUT`.
