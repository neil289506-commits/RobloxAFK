#Requires AutoHotkey v2.0
#SingleInstance Force
; args: key action holdMs taps intervalSec stopFile
key := A_Args[1], action := A_Args[2], holdMs := Integer(A_Args[3])
taps := Integer(A_Args[4]), intervalMs := Integer(A_Args[5]) * 1000, stopFile := A_Args[6]
target := "ahk_exe RobloxPlayerBeta.exe"

OnExit((*) => Send("{" key " up}"))
^!q::ExitApp  ; 緊急停止：Ctrl+Alt+Q

Stopping() => FileExist(stopFile) != ""

Pause_(ms) {
    t := A_TickCount
    while (A_TickCount - t < ms && !Stopping())
        Sleep 50
}

Focus() {
    if !WinExist(target)
        return false
    WinActivate(target)
    return WinWaitActive(target, , 2) != 0
}

Cycle() {
    if !Focus()
        return
    if (action = "hold") {
        Send "{" key " down}"
        Pause_(holdMs)
        Send "{" key " up}"
    } else {
        Loop taps {
            Send "{" key " down}"
            Pause_(80)
            Send "{" key " up}"
            Pause_(120)
            if Stopping()
                break
        }
    }
}

Loop {
    Cycle()
    if Stopping()
        break
    Pause_(intervalMs)
    if Stopping()
        break
}
ExitApp
