#!/usr/bin/env python3
"""Interactive CLI tester for the CyberKey firmware wire protocol.

Usage:
    python3 test.py [PORT]

PORT defaults to the first /dev/cu.usbserial-* device found.
Special substitution: $now is replaced by the current Unix timestamp, e.g.:
    {"cmd":"sync_clock","ts":$now}
"""
import glob, serial, sys, time

port = sys.argv[1] if len(sys.argv) > 1 else next(iter(glob.glob("/dev/cu.usbserial-*")), None)
if not port:
    sys.exit("No serial port found. Plug in the device or pass PORT as argument.")

s = serial.Serial(port, 115200, timeout=1)
print(f"Connected to {port}. Type JSON commands, Ctrl-C to quit.")
print("  $now is replaced by the current Unix timestamp.")

while True:
    try:
        cmd = input("> ")
    except (EOFError, KeyboardInterrupt):
        print()
        break
    cmd = cmd.replace("$now", str(int(time.time())))
    s.write((cmd + "\n").encode())
    time.sleep(0.4)
    while s.in_waiting:
        line = s.readline().decode(errors="replace").strip()
        if line.startswith("{"):
            print(line)
