// Run with go run test-fixtures/phase2/extract-health-durations.go.
// This is the frozen reference's containerDuration predicate, without host tools.
package main

import (
	"encoding/json"
	"os"
	"runtime"
	"time"
)

func main() {
	values := []string{"", "0", "+", "-1s", "tomorrow", "1d", "1..2s", ".s", "1s ", "1s-2s", "1h30m", "1.s", "1µs", "1μs", "9223372036854775807ns", "9223372036854775808ns", "2562047h47m16.854775807s", "2562047h47m16.854775808s", "99999999999999999999999999999999999999999h"}
	for _, number := range []string{"0", "1", "+.5", "0.000000001", "0.0000000001", "1.2345678901234567890123456789"} {
		for _, unit := range []string{"ns", "us", "µs", "ms", "s", "m", "h"} {
			values = append(values, number+unit)
		}
	}
	type item struct {
		Value string `json:"value"`
		Valid bool   `json:"valid"`
	}
	cases := []item{}
	for _, value := range values {
		duration, err := time.ParseDuration(value)
		cases = append(cases, item{value, value == "" || (err == nil && duration > 0)})
	}
	result := map[string]any{"reference": "c1a9f32f5898146ae7ac339043b910b8e2a2c438", "source": "internal/declarative/container_options.go:containerDuration", "method": "standalone copy of the frozen reference predicate; Go standard-library evaluation", "toolchain": runtime.Version(), "cases": cases}
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(result); err != nil {
		panic(err)
	}
}
