"""Exercise the native Windows client from the Windows live provider workflow."""

import os
from pathlib import Path
from queue import Empty, Queue
import subprocess
from threading import Thread
from time import monotonic

from winpty import PtyProcess


def console(args, ready, respond, expected, marker=""):
    """Type only after the remote prompt or command is ready, then check its exit."""
    child = PtyProcess.spawn(["boatparty.exe", *args])
    chunks = Queue()

    def read():
        try:
            while True:
                chunks.put(child.read())
        except EOFError:
            pass
        finally:
            chunks.put(None)

    Thread(target=read, daemon=True).start()
    transcript = ""
    sent = False
    deadline = monotonic() + 90
    try:
        while True:
            try:
                piece = chunks.get(timeout=max(0, deadline - monotonic()))
            except Empty:
                raise RuntimeError(f"Timed out in {args[0]}: {transcript[-500:]}") from None
            if piece is None:
                break
            transcript += piece
            if not sent and ready(transcript):
                respond(child)
                sent = True
        status = child.wait()
        if not sent or marker not in transcript or status != expected:
            raise RuntimeError(f"{args[0]} exited {status}: {transcript[-500:]}")
    finally:
        child.close(force=True)


def main():
    os.chdir(os.environ["TEST_CHECKOUT"])
    project = os.environ["TEST_PROJECT"]
    subprocess.run(["git", "init", "-q"], check=True)
    Path("tracked.txt").write_bytes(b"original\n")
    subprocess.run(["git", "add", "."], check=True)
    subprocess.run(
        ["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
         "commit", "-qm", "initial"],
        check=True,
    )

    def cli(*args, **kwargs):
        return subprocess.run(["boatparty", *args], check=True, **kwargs)

    cli("doctor")
    cli("init", "--name", project)
    cli("limits", "set", "account", "paid_starts=false")
    cli("limits", "set", "project", project, "max_sandboxes=1", "max_running=1",
        "max_machine=default", "starts_per_hour=1", "starts_per_day=1")
    cli("start")
    cli("sync")
    cli("run", "--sh", "printf updated > tracked.txt")
    if Path("tracked.txt").read_bytes() != b"updated":
        raise RuntimeError("Copy-back did not update the Windows checkout")
    if cli("shell", "--", "cat", "tracked.txt", stdout=subprocess.PIPE).stdout != b"updated":
        raise RuntimeError("shell did not read the sandbox checkout")
    cli("status")

    def type_shell_command(child):
        child.write("test -t 0 && test -t 1 && printf 'TTY-%s\\n' ACTIVE && exit 23\r")

    console(["shell"], lambda text: "$" in text or "#" in text,
            type_shell_command, 23, "TTY-ACTIVE")
    console(["run", "--sh", "echo CANCEL-READY; exec sleep 120"],
            lambda text: "CANCEL-READY" in text, lambda child: child.sendintr(), 130)
    cli("stop", "--wait")


if __name__ == "__main__":
    main()
