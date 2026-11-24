# Upstream contract extraction (temporary porting reference)

The following Go program runs **in a checkout of the pinned Server Man upstream**, not in Maestro. Copy it to the upstream repository root as a temporary Go source file, run it there, and remove it afterwards. Capture its JSON output, replace upstream product wording in descriptions with Maestro wording, and compare with `test-fixtures/phase2/maestro-tools.json`. The fixture tests Maestro's tool schemas; it is not an executable upstream dependency.

```go
package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"server-man/internal/app"
	"server-man/internal/config"
	"server-man/internal/core"
	"server-man/internal/modules/monitor"
)

type denied struct{}

func (denied) Run(context.Context, core.Command, io.Writer) error {
	return errors.New("contract extraction forbids commands")
}
func (denied) Capture(context.Context, core.Command) (string, error) {
	return "", errors.New("contract extraction forbids commands")
}
func main() {
	cfg, err := config.Defaults()
	if err != nil {
		panic(err)
	}
	dir, err := os.MkdirTemp("", "maestro-reference-contract-")
	if err != nil {
		panic(err)
	}
	defer os.RemoveAll(dir)
	cfg.Path = dir + "/server-man.yaml"
	cfg.StateDir = dir
	jobs, err := core.NewJobs(context.Background(), dir)
	if err != nil {
		panic(err)
	}
	modes := map[string]any{}
	for _, helper := range []bool{false, true} {
		cfg.PrivilegedOperations = helper
		registry, err := app.Registry(cfg, jobs, &monitor.Sampler{}, denied{})
		if err != nil {
			panic(err)
		}
		key := "without_helper"
		if helper {
			key = "with_helper"
		}
		modes[key] = registry.Tools()
	}
	out := json.NewEncoder(os.Stdout)
	out.SetIndent("", "  ")
	if err := out.Encode(map[string]any{"reference": "c1a9f32f5898146ae7ac339043b910b8e2a2c438", "method": "app.Registry with non-nil monitor and command-denying Executor; no tools executed", "modes": modes}); err != nil {
		panic(err)
	}
}

```
