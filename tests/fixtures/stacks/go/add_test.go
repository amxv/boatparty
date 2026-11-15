package fixture

import (
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestAddWithTheDependency(t *testing.T) {
	if diff := cmp.Diff(5, Add(2, 3)); diff != "" {
		t.Fatal(diff)
	}
}
