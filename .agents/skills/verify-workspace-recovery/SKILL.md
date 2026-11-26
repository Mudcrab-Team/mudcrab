---
name: verify-workspace-recovery
description: Check workspace.py build retirement, incremental trimming, and recovery after a process stops. Run when changing cleanup receipts, rename order, deletion, or retry behavior.
---

# Verify workspace crash recovery

Recovery must finish removing a renamed build or incremental directory while preserving live output and unmarked directories outside that removal. A stale receipt once let recovery delete someone's directory after an unrelated cleanup of the same slot.

From the repository root on Linux with Git and CPython 3.14, run:

```sh
python3 -I -B .agents/skills/verify-workspace-recovery/check.py --history
```

The history check needs commits `2bddabda` (#797) and `8eb68b15` (#800) in the local clone. Omit `--history` to check only the working copy in a shallow clone. CI runs both checks.

## Read the result

Expect four `EXPECTED FAILURE` lines and a `PASS` for both retire and trim. At #797, recovery deleted an unmarked directory on its first pass. At #800, completing one legitimate removal left a receipt that authorized a different directory. The checker runs those revisions against temporary files and requires the personal file to disappear. An unrelated exception fails the run.

On a current failure, read the cleanup and recovery operations in order. A crash after `rename` can leave a whole directory to remove. A crash after `unlink build.json` can leave a directory that only its receipt identifies. Check the reported path against the removal recorded in `evidence/`.

## Bounds

The checker runs the production Python code on a real filesystem. It tries every prefix of successful `os.replace`, `os.rename`, `os.unlink`, and `os.rmdir` calls in one cleanup, then every prefix of one recovery from that state. Two uninterrupted retries with fresh Workspace instances must finish the removal and preserve file contents.

One slot suffices to expose a stale receipt for the same name. Two profiles suffice to stop a trim between renames. A third, unselected profile tests a lookalike with the real trim's timestamp. A marker and a payload expose partial deletion of a retired build. Recreating a missing source before retry checks that a new build survives. The run requires both a markerless retired directory and a partially renamed trim to be reached.

Assume an idle private store on one filesystem and atomic rename. Writers may recreate the live build name. The bound excludes concurrent writers replacing a recorded leftover at its exact path, forged receipts or build markers, power loss, and failures within a system call. The checker supplies an idle consumer result. Run the separate workspace suite for validation, leases, open files, symlinks, devices, and I/O errors:

```sh
python3 -I -B -m unittest discover -s scripts/tests -p 'test_workspace*.py'
```

## Keep the check in sync

Last checked against production commit `f136e48662728adda47cf62caec4c87fbe427403`. In `scripts/workspace.py`, `retire_leased` and `trim_leased` publish the receipt and rename its directories. `has_recovery_evidence`, `recover_interrupted`, and `delete_leftover` decide which deletions can resume.

After a protocol change, compare its persistent mutations with the four instrumented calls. Extend the crash points if a new operation can change recoverable state. Reproduce the symptom with the production code before changing the expected result. Advance the sync commit after the historical cases still lose the file and the working copy passes. Use `/maintain-formal-verification-skill` after a change that missed this check.
