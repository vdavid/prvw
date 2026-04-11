package main

import (
	"fmt"
	"os"
	"path/filepath"
)

// findRootDir finds the project root directory by looking for AGENTS.md.
func findRootDir() (string, error) {
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}

	for {
		agentsMd := filepath.Join(dir, "AGENTS.md")
		if _, err := os.Stat(agentsMd); err == nil {
			return dir, nil
		}

		parent := filepath.Dir(dir)
		if parent == dir {
			return "", fmt.Errorf("could not find project root (looking for AGENTS.md)")
		}
		dir = parent
	}
}
