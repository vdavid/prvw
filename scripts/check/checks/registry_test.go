package checks

import (
	"testing"
)

func TestValidateCheckNames_NoCollisions(t *testing.T) {
	if err := ValidateCheckNames(); err != nil {
		t.Errorf("ValidateCheckNames() failed on actual registry: %v", err)
	}
}

func TestValidateCheckNames_DetectsNicknameIDCollision(t *testing.T) {
	original := AllChecks
	defer func() { AllChecks = original }()

	AllChecks = []CheckDefinition{
		{ID: "check-a", Nickname: "short-a", DisplayName: "A", App: AppDesktop, Tech: "Test"},
		{ID: "short-a", DisplayName: "B", App: AppDesktop, Tech: "Test"},
	}

	err := ValidateCheckNames()
	if err == nil {
		t.Error("ValidateCheckNames() should detect nickname-ID collision")
	}
}

func TestValidateCheckNames_DetectsDuplicateNicknames(t *testing.T) {
	original := AllChecks
	defer func() { AllChecks = original }()

	AllChecks = []CheckDefinition{
		{ID: "check-a", Nickname: "short", DisplayName: "A", App: AppDesktop, Tech: "Test"},
		{ID: "check-b", Nickname: "short", DisplayName: "B", App: AppDesktop, Tech: "Test"},
	}

	err := ValidateCheckNames()
	if err == nil {
		t.Error("ValidateCheckNames() should detect duplicate nicknames")
	}
}

func TestCLIName(t *testing.T) {
	tests := []struct {
		name     string
		def      CheckDefinition
		expected string
	}{
		{
			name:     "returns nickname when set",
			def:      CheckDefinition{ID: "full-id", Nickname: "short"},
			expected: "short",
		},
		{
			name:     "returns ID when nickname is empty",
			def:      CheckDefinition{ID: "full-id", Nickname: ""},
			expected: "full-id",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.def.CLIName(); got != tt.expected {
				t.Errorf("CLIName() = %v, want %v", got, tt.expected)
			}
		})
	}
}
