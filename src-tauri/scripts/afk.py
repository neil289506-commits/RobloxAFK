# -*- coding: utf-8 -*-
# args: key action holdMs taps intervalSec stopFile pid
import os, sys, time, ctypes

KEY, ACTION = sys.argv[1], sys.argv[2]
HOLD_MS, TAPS, INTERVAL = int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
STOP, PID = sys.argv[6], int(sys.argv[7])

try:
    from pywinauto import findwindows
    from pywinauto.controls.hwndwrapper import HwndWrapper
except ImportError:
    sys.exit("找不到 pywinauto，請先在程式內按「安裝」")

WM_KEYDOWN, WM_KEYUP = 0x0100, 0x0101
NAMED = {"space": 0x20, "enter": 0x0D, "tab": 0x09, "esc": 0x1B, "escape": 0x1B,
         "shift": 0x10, "ctrl": 0x11, "left": 0x25, "up": 0x26, "right": 0x27, "down": 0x28}
EXTENDED = {0x25, 0x26, 0x27, 0x28}


def to_vk(name):
    n = name.lower()
    if n in NAMED:
        return NAMED[n]
    if len(n) == 1 and n.isalnum():
        return ord(n.upper())
    if n.startswith("f") and n[1:].isdigit() and 1 <= int(n[1:]) <= 12:
        return 0x70 + int(n[1:]) - 1
    sys.exit("不支援的按鍵：" + name)


VK = to_vk(KEY)
SCAN = ctypes.windll.user32.MapVirtualKeyW(VK, 0)
EXT = (1 << 24) if VK in EXTENDED else 0
L_DOWN = 1 | (SCAN << 16) | EXT
L_UP = 1 | (SCAN << 16) | EXT | (1 << 30) | (1 << 31)


def stopping():
    return os.path.exists(STOP)


def wait(sec):
    end = time.time() + sec
    while time.time() < end and not stopping():
        time.sleep(0.05)


def wins():
    return [HwndWrapper(h) for h in findwindows.find_windows(process=PID)]


def send(ws, msg, lp):
    for w in ws:  # PostMessage：不需要視窗取得焦點
        w.post_message(msg, VK, lp)


def cycle():
    ws = wins()
    if not ws:
        return
    try:
        if ACTION == "hold":
            send(ws, WM_KEYDOWN, L_DOWN)
            wait(HOLD_MS / 1000)
        else:
            for _ in range(TAPS):
                send(ws, WM_KEYDOWN, L_DOWN)
                wait(0.08)
                send(ws, WM_KEYUP, L_UP)
                wait(0.12)
                if stopping():
                    break
    finally:
        send(ws, WM_KEYUP, L_UP)


if not wins():
    sys.exit("找不到 PID %d 的可見視窗，請確認 PID 是否正確" % PID)

while not stopping():
    cycle()
    wait(INTERVAL)
