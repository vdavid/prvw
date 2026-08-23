//go:build !windows

package main

// prepareConsole is a no-op outside Windows, where terminals handle UTF-8 and
// ANSI escape sequences without being asked.
func prepareConsole() {}
