"""比较直接启动与 niri IPC 启动同一原生程序，到达 main 的时间。输出 CSV。"""

import csv
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import tempfile
import time


MARKER = """\
#include <fcntl.h>
#include <stdint.h>
#include <time.h>
#include <unistd.h>
int main(int argc, char **argv) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0 || argc != 2) return 1;
    int fd = open(argv[1], O_WRONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) return 2;
    int64_t ns = (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
    return write(fd, &ns, sizeof(ns)) == sizeof(ns) ? 0 : 3;
}
"""


def measure(route, command, fd):
    if route == "direct":
        start = time.monotonic_ns()
        subprocess.run(command, check=True)
    else:
        request = json.dumps({"Action": {"Spawn": {"command": command}}}).encode() + b"\n"
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(2)
            sock.connect(os.environ["NIRI_SOCKET"])
            start = time.monotonic_ns()
            sock.sendall(request)
            with sock.makefile("rb") as reply:
                if "Ok" not in json.loads(reply.readline()):
                    raise RuntimeError("niri 拒绝启动测量程序")
    if not select.select([fd], [], [], 2)[0]:
        raise TimeoutError("未收到程序入口时间戳")
    end, = struct.unpack("=q", os.read(fd, 8))
    return (end - start) / 1_000_000


def main():
    with tempfile.TemporaryDirectory(dir=os.environ["XDG_RUNTIME_DIR"]) as directory:
        root = Path(directory)
        source, binary, fifo = (root / name for name in ("mark.c", "mark", "time.fifo"))
        source.write_text(MARKER)
        subprocess.run(["cc", "-O2", str(source), "-o", str(binary)], check=True)
        command = [str(binary), str(fifo)]
        os.mkfifo(fifo, 0o600)
        fd = os.open(fifo, os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC)
        try:
            writer = csv.writer(sys.stdout, lineterminator="\n")
            writer.writerow(["block", "route", "entry_ms"])
            for block in range(49):
                order = ("direct", "ipc") if block % 2 else ("ipc", "direct")
                for route in order:
                    writer.writerow([block, route, f"{measure(route, command, fd):.6f}"])
                    time.sleep(0.105)
        finally:
            os.close(fd)


if __name__ == "__main__":
    main()
