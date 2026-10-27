package main

import (
	"fmt"
	"math/big"
	"regexp"
	"strings"
)

var decimalNumber = regexp.MustCompile(`^(?:0|[1-9][0-9]*(?:_[0-9]+)*)(?:\.(?:[0-9]+(?:_[0-9]+)*)?)?(?:[eE][+-]?[0-9]+(?:_[0-9]+)*)?$|^\.[0-9]+(?:_[0-9]+)*(?:[eE][+-]?[0-9]+(?:_[0-9]+)*)?$`)
var radixNumber = regexp.MustCompile(`^(?:0[xX][0-9a-fA-F]+(?:_[0-9a-fA-F]+)*|0[oO][0-7]+(?:_[0-7]+)*|0[bB][01]+(?:_[01]+)*)$`)

func parseFieldNumber(raw string) (int, error) {
	raw = strings.TrimSpace(raw)
	invalid := func() (int, error) { return 0, fmt.Errorf("invalid protobuf field number: %s", raw) }
	var integer *big.Int
	if radixNumber.MatchString(raw) {
		integer = new(big.Int)
		if _, ok := integer.SetString(strings.ReplaceAll(raw, "_", ""), 0); !ok {
			return invalid()
		}
	} else {
		if !decimalNumber.MatchString(raw) {
			return invalid()
		}
		number := strings.ReplaceAll(raw, "_", "")
		if i := strings.IndexAny(number, "eE"); i >= 0 {
			exponent, ok := new(big.Int).SetString(strings.TrimPrefix(number[i+1:], "+"), 10)
			if !ok || !exponent.IsInt64() || exponent.Int64() > 1000 || exponent.Int64() < -1000 {
				return invalid()
			}
		}
		r, ok := new(big.Rat).SetString(number)
		if !ok || !r.IsInt() {
			return invalid()
		}
		integer = r.Num()
	}
	if !integer.IsInt64() {
		return invalid()
	}
	n := integer.Int64()
	if n < 1 || n > 536870911 || (n >= 19000 && n <= 19999) {
		return invalid()
	}
	return int(n), nil
}
