//go:build !windows && !darwin

package cursor

import "fmt"

// EnsureCACertInstalled 非 Windows/macOS 平台的存根实现
func EnsureCACertInstalled(_ []byte, certPath string) error {
	return fmt.Errorf("ensureCACertInstalled: 当前平台暂不支持，certPath=%s", certPath)
}

// RemoveCACertInstalled 非 Windows/macOS 平台无需操作系统证书清理。
func RemoveCACertInstalled(_ []byte) error {
	return nil
}
