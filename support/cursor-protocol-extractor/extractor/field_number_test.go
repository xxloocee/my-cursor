package main

import (
	"fmt"
	"testing"
)

func TestFieldNumberLiterals(t *testing.T) {
	for _, tc := range []struct {
		s    string
		want int
	}{
		{"1", 1}, {"1.", 1}, {"1.e3", 1000}, {"1e3", 1000}, {"1.0e3", 1000}, {"0x20", 32}, {"0o20", 16}, {"0b10", 2}, {"1_000", 1000}, {".1e2", 10}, {"536870911", 536870911},
		{"0", 0}, {"19000", 0}, {"19999", 0}, {"536870912", 0}, {"1.1", 0}, {"1e-3", 0}, {"1+2", 0}, {"1abc", 0}, {"NaN", 0}, {"Infinity", 0}, {"1__0", 0}, {"1e999999", 0}, {"9007199254740993e-10", 0},
	} {
		t.Run(tc.s, func(t *testing.T) {
			f, err := parseFieldObject(fmt.Sprintf(`{no:%s,name:"field",kind:"scalar",T:5}`, tc.s))
			if tc.want == 0 {
				if err == nil {
					t.Fatalf("accepted invalid literal: %#v", f)
				}
			} else if err != nil || f.No != tc.want {
				t.Fatalf("got %#v, %v; want %d", f, err, tc.want)
			}
		})
	}
}
