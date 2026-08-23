//go:build windows

package main

import (
	"os"

	"golang.org/x/sys/windows"
)

// utf8CodePage is Windows' identifier for UTF-8.
const utf8CodePage = 65001

// prepareConsole makes the runner's output readable on Windows: UTF-8 so the
// emoji in check names arrive as emoji, and virtual terminal processing so the
// colors and the status line's escape sequences are rendered rather than
// printed. Windows Terminal does both already, but conhost, still what cmd.exe
// opens on Windows 10, does neither.
//
// The code page is console-wide and outlives the run. That is the accepted cost
// of the fix: every exit path here goes through os.Exit, so a deferred restore
// would not run anyway.
//
// Both calls fail harmlessly when stdout is redirected to a file or a pipe,
// because there is no console to configure.
func prepareConsole() {
	_ = windows.SetConsoleOutputCP(utf8CodePage)

	stdout := windows.Handle(os.Stdout.Fd())
	var mode uint32
	if err := windows.GetConsoleMode(stdout, &mode); err != nil {
		return
	}
	_ = windows.SetConsoleMode(stdout, mode|windows.ENABLE_VIRTUAL_TERMINAL_PROCESSING)
}
