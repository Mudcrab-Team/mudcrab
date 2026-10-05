"""Verified inputs, guarded artifacts, and process deadlines for P0 tool probes."""
from __future__ import annotations

import hashlib
import locale
import os
import signal
import subprocess
import sys
from pathlib import Path

import corpus_manifest

RE_PROJECT_ROOT = Path("/home/dev/.t3/projects/mudcrab-reverse-engineering")
MCRAB_STORE = Path("/home/dev/mcrab-store")
REPO_ROOT = Path(__file__).resolve().parents[2]


def _sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


class QualificationError(RuntimeError):
    pass


class ProcessTimeout(QualificationError):
    def __init__(self, label: str, timeout: float, stdout: str, stderr: str):
        super().__init__(f"{label} timed out after {timeout:g} seconds")
        self.label = label
        self.timeout = timeout
        self.stdout = stdout
        self.stderr = stderr


class ProcessOutputDecodeError(QualificationError):
    """Child output was not valid text in the runner's current locale."""

    def __init__(
        self,
        label: str,
        encoding: str,
        stdout: bytes,
        stderr: bytes,
        *,
        stream: str,
        error: UnicodeDecodeError,
        timeout: float | None = None,
    ):
        outcome = f"{label} output has undecodable {stream} bytes"
        if timeout is not None:
            outcome = f"{label} timed out after {timeout:g} seconds and produced undecodable {stream} bytes"
        super().__init__(
            f"{outcome} using {encoding} at bytes {error.start}:{error.end}: {error.reason}"
        )
        self.label = label
        self.encoding = encoding
        self.stdout_bytes = stdout
        self.stderr_bytes = stderr
        self.stream = stream
        self.byte_start = error.start
        self.byte_end = error.end
        self.reason = error.reason
        self.timeout = timeout

    def failure_record(self) -> dict:
        return {
            "encoding": self.encoding,
            "stream": self.stream,
            "byte_start": self.byte_start,
            "byte_end": self.byte_end,
            "reason": self.reason,
            "timed_out": self.timeout is not None,
            "raw_stdout_sha256": _sha256(self.stdout_bytes),
            "raw_stdout_size_bytes": len(self.stdout_bytes),
            "raw_stderr_sha256": _sha256(self.stderr_bytes),
            "raw_stderr_size_bytes": len(self.stderr_bytes),
        }


def _read_verified_bytes(path: Path, *, max_bytes: int = 32 * 1024 * 1024) -> tuple[str, int, bytes]:
    try:
        requested = path.lstat().st_size
    except OSError as exc:
        raise QualificationError(f"cannot stat pinned source {path}: {exc}") from exc
    if requested > max_bytes:
        raise QualificationError(f"pinned source exceeds {max_bytes}-byte capture limit: {path}")
    try:
        digest, size, raw = corpus_manifest._read_verified_file(path, capture_bytes=requested)
    except (OSError, corpus_manifest.SourceDriftError) as exc:
        raise QualificationError(f"pinned source verification failed for {path}: {exc}") from exc
    if len(raw) != size:
        raise QualificationError(
            f"verified read did not capture the full source {path}: captured {len(raw)}, read {size}"
        )
    if _sha256(raw) != digest:
        raise QualificationError(f"verified source bytes and digest differ for {path}")
    return digest, size, raw


def runner_provenance(runner: Path) -> dict:
    """Identify the decision code separately from the external tool binary."""
    paths = (runner, Path(__file__), Path(corpus_manifest.__file__), runner.parent / "p0_fixtures.py")
    files = []
    for path in paths:
        digest, size, _ = _read_verified_bytes(path)
        files.append({"path": str(path.resolve().relative_to(REPO_ROOT)), "sha256": digest, "size": size})
    return {"python": sys.version, "files": files}


def validate_artifact_destination(
    candidate: Path,
    oracle_source: Path,
    *,
    additional_protected: tuple[Path, ...] = (),
) -> None:
    candidate = candidate.expanduser().resolve()
    protected = _repository_worktree_roots() + (RE_PROJECT_ROOT, MCRAB_STORE, oracle_source.expanduser().resolve()) + tuple(
        path.expanduser().resolve() for path in additional_protected
    )
    for protected_root in protected:
        if _contains(candidate, protected_root) or _contains(protected_root, candidate):
            raise QualificationError(
                f"artifact directory overlaps protected path {protected_root}: {candidate}"
            )


def _repository_worktree_roots() -> tuple[Path, ...]:
    """Guard all registered checkouts, including the primary dirty checkout."""
    if not (REPO_ROOT / ".git").exists():
        return (REPO_ROOT,)
    try:
        result = subprocess.run(
            ["git", "-C", str(REPO_ROOT), "worktree", "list", "--porcelain", "-z"],
            check=True, capture_output=True, timeout=5,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise QualificationError(f"cannot verify protected repository worktrees: {exc}") from exc
    roots = tuple(
        Path(os.fsdecode(field[len(b"worktree "):])).resolve()
        for field in result.stdout.split(b"\0") if field.startswith(b"worktree ")
    )
    if REPO_ROOT not in roots:
        raise QualificationError("registered worktree list omitted the current repository")
    return roots


def _contains(parent: Path, child: Path) -> bool:
    try:
        child.relative_to(parent)
        return True
    except ValueError:
        return False


def _run_supervised(
    command: list[str], *, cwd: Path, env: dict[str, str], label: str, timeout: float
) -> subprocess.CompletedProcess:
    process = subprocess.Popen(
        command,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=(os.name == "posix"),
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        if os.name == "posix":
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        else:
            try:
                subprocess.run(
                    ["taskkill", "/T", "/F", "/PID", str(process.pid)],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    timeout=2, check=False,
                )
            except (OSError, subprocess.TimeoutExpired):
                pass
            if process.poll() is None:
                process.kill()
        try:
            stdout, stderr = process.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            if os.name == "posix":
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            else:
                process.kill()
            try:
                stdout, stderr = process.communicate(timeout=2)
            except subprocess.TimeoutExpired as exc:
                # A detached descendant can retain these pipes after the
                # supervised process group has died. Keep captured evidence
                # and close our readers instead of waiting for that descendant.
                stdout, stderr = exc.output, exc.stderr
                # Windows readers can own the pipe lock until a descendant
                # exits. Closing those streams here could block this thread.
                if os.name == "posix":
                    process.stdout.close()
                    process.stderr.close()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    pass
        output = _decode_process_output(
            label, stdout or b"", stderr or b"", timeout=timeout
        )
        raise ProcessTimeout(label, timeout, output[0], output[1])
    decoded_stdout, decoded_stderr = _decode_process_output(
        label, stdout or b"", stderr or b""
    )
    return subprocess.CompletedProcess(
        command, process.returncode, decoded_stdout, decoded_stderr
    )


def _output_encoding() -> str:
    if sys.flags.utf8_mode:
        return "utf-8"
    getencoding = getattr(locale, "getencoding", None)
    return getencoding() if getencoding else locale.getpreferredencoding(False)


def _decode_process_output(
    label: str, stdout: bytes, stderr: bytes, *, timeout: float | None = None
) -> tuple[str, str]:
    encoding = _output_encoding()
    decoded: list[str] = []
    for stream, raw in (("stdout", stdout), ("stderr", stderr)):
        try:
            decoded.append(raw.decode(encoding))
        except UnicodeDecodeError as exc:
            raise ProcessOutputDecodeError(
                label, encoding, stdout, stderr, stream=stream, error=exc, timeout=timeout
            ) from exc
    return decoded[0], decoded[1]
