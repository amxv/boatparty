# Commands share this lock. Pruning takes it exclusively only to revalidate
# and rename ONE stale entry. Walking, sorting and recursive deletion all
# happen after the command, outside both this lock and the checkout lock.
cache_in_use() {
  (("${#bp_cache_dirs[@]}" > 0)) || return 0
  if command -v flock >/dev/null 2>&1; then
    exec 5>"$state/cache-use.lock"
    flock -s 5
  fi
}

# After a builder warm-up the optional timestamp additionally discards
# entries last used before that warm-up. Linux /proc lets rm operate
# relative to an open directory, even if a checkout path changes.
trim_caches() {
  (("${#bp_cache_dirs[@]}" && "${#bp_output_dirs[@]}")) || return 0
  [[ -d /proc/self/fd ]] || return 0
  python3 - "$repo" "$state/cache-use.lock" "$bp_cache_days" "$bp_min_free_gb" "${1:-}" \
    "${#bp_output_dirs[@]}" "${bp_output_dirs[@]}" "${bp_cache_dirs[@]}" <<'PY_CACHE'
import contextlib
import fcntl
import fnmatch
import glob
import os
import re
import stat
import subprocess
import sys
import time
import uuid

repo, lock_path, days, floor, stamp, count, *patterns = sys.argv[1:]
days, floor, count = int(days), int(floor) * 1000**3, int(count)
cutoff = time.time() - days * 86400 if days else 0
if stamp and os.path.isfile(stamp):
    cutoff = max(cutoff, os.stat(stamp).st_mtime)
if not cutoff and not floor:
    sys.exit(0)
os.chdir(repo)
flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
entries = {}
roots = {}
seen = set()
# A configured cache may contain tracked fixtures. Protect the entire entry
# containing a source; pruning must never send a source deletion back.
tracked = set()
files = subprocess.check_output(["git", "ls-files", "-z"]).split(b"\0")
for file in files:
    parts = tuple(os.fsdecode(file).split("/"))
    tracked.update(parts[:i] for i in range(1, len(parts) + 1))

garbage_prefix = ".boatparty-pruning-"

def mount(fd):
    with open(f"/proc/self/fdinfo/{fd}") as info:
        return next(line for line in info if line.startswith("mnt_id:"))

def collect(path, depth, stack):
    if (path, depth) in seen or path in roots or ".git" in path or ".." in path:
        return
    seen.add((path, depth))
    with contextlib.ExitStack() as parents:
        fd = os.open(".", flags)
        parents.callback(os.close, fd)
        for component in path:
            fd = os.open(component, flags, dir_fd=fd)
            parents.callback(os.close, fd)
            if mount(fd) != checkout_mount:
                return
        if path and any(fnmatch.fnmatchcase(path[-1], p) for p in patterns[count:]):
            root = os.dup(fd)
            stack.callback(os.close, root)
            roots[path] = root
            for name in os.listdir(fd):
                if (*path, name) in tracked:
                    continue
                try:
                    s = os.stat(name, dir_fd=fd, follow_symlinks=False)
                except FileNotFoundError:
                    continue
                entries[path, name] = (max(s.st_atime_ns, s.st_mtime_ns), s.st_ino)
        elif depth < 4:
            for name in os.listdir(fd):
                try:
                    if stat.S_ISDIR(os.stat(name, dir_fd=fd, follow_symlinks=False).st_mode):
                        collect((*path, name), depth + 1, stack)
                except OSError:
                    continue

def available(fd):
    space = os.fstatvfs(fd)
    return space.f_bavail * space.f_frsize

with contextlib.ExitStack() as stack, open(lock_path, "a") as lock:
    checkout = os.open(".", flags)
    stack.callback(os.close, checkout)
    checkout_mount = mount(checkout)
    for pattern in patterns[:count]:
        for root in glob.iglob(pattern):
            if os.path.isabs(root):
                continue
            try:
                collect(tuple(c for c in root.split("/") if c not in ("", ".")), 0, stack)
            except OSError:
                continue
    for (path, name), (used, ino) in sorted(entries.items(), key=lambda item: item[1]):
        fd = roots[path]
        if not name.startswith(garbage_prefix) and used >= cutoff * 1e9 and available(fd) >= floor:
            continue
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            break
        try:
            s = os.stat(name, dir_fd=fd, follow_symlinks=False)
            if s.st_ino != ino or max(s.st_atime_ns, s.st_mtime_ns) != used:
                continue
            trash = garbage_prefix + uuid.uuid4().hex
            os.rename(name, trash, src_dir_fd=fd, dst_dir_fd=fd)
        except OSError:
            continue
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)
        # GNU rm --one-file-system cannot distinguish a bind mount on the
        # same device. Refuse trees containing any nested mount point.
        trash_path = os.path.realpath(f"/proc/self/fd/{fd}/{trash}")
        with open("/proc/self/mountinfo") as mounts:
            mounted = [re.sub(r"\\([0-7]{3})", lambda m: chr(int(m[1], 8)), line.split()[4])
                       for line in mounts]
        if not stat.S_ISLNK(s.st_mode) and any(m == trash_path or m.startswith(trash_path + "/") for m in mounted):
            os.rename(trash, name, src_dir_fd=fd, dst_dir_fd=fd)
            continue
        removed = subprocess.run(["rm", "-rf", "--one-file-system", "--", f"/proc/self/fd/{fd}/{trash}"],
                                 pass_fds=(fd,), check=False)
        if removed.returncode == 0:
            print(f"boatparty: pruned cache entry {os.path.join(*path, name)!r}", file=sys.stderr)
    if available(checkout) < floor:
        print(f"boatparty: free space is still below the {floor // 1000**3} GB cache floor", file=sys.stderr)
PY_CACHE
}
